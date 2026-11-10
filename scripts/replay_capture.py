"""Private, integrity-checked producer responses for oracle-only replay.

Capture always executes producers, including repeated requests. Repeated exact
requests must agree; replay is a lookup, not an ordered execution transcript.
Replay completion requires consuming every captured unique request.
Hashes detect corruption, not malicious rewriting of the entire capture.
Oracle stdout/stderr and JSON reports contain unredacted document words and
paths. Store them only in owner-only paths (0700 directories / 0600 files),
just like --capture directories and their contents.
"""

import hashlib
import json
import locale
import os
import re
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

CODE_SHA256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def _json_bytes(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def _digest(data):
    return hashlib.sha256(data).hexdigest()


class ReplayCapture:
    """Record text-mode subprocess results, or replay a completed capture.

    The caller owns producer/input identity and must recheck it before finish().
    No subprocess is launched in replay mode. Files contain unredacted private
    inputs and outputs and are created in a new owner-only directory.
    """

    def __init__(self, directory: Path, identity: dict, replay: bool = False):
        self.directory = Path(directory).absolute()
        if not isinstance(identity, dict):
            raise TypeError("capture identity must be a dictionary")
        self.identity = json.loads(_json_bytes(identity))
        self.replay = replay
        self._failed = False
        self._finished = False
        self._lock = threading.Lock()
        self._active = 0
        self._responses = {}
        self._used = set()
        if replay:
            self._load_manifest()
        else:
            # Never reuse or overwrite even an empty existing directory.
            self.directory.mkdir(mode=0o700)
            os.chmod(self.directory, 0o700)
            self._checkpoint(False)

    def _atomic_json(self, path, value):
        data = _json_bytes(value)
        fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=self.directory)
        try:
            with os.fdopen(fd, "wb") as stream:
                os.fchmod(stream.fileno(), 0o600)
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
            directory_fd = os.open(self.directory, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        return _digest(data)

    def _checkpoint(self, complete):
        payload = {"version": 1, "identity": self.identity,
                   "complete": complete, "responses": self._responses}
        self._atomic_json(self.directory / "manifest.json",
                          {"payload": payload, "sha256": _digest(_json_bytes(payload))})

    def _read_json(self, path):
        try:
            data = path.read_bytes()
            return data, json.loads(data)
        except (OSError, ValueError) as error:
            raise RuntimeError(f"missing or corrupt capture file: {path.name}") from error

    def _load_manifest(self):
        _, envelope = self._read_json(self.directory / "manifest.json")
        try:
            if set(envelope) != {"payload", "sha256"}:
                raise ValueError("invalid manifest envelope")
            payload = envelope["payload"]
            if envelope["sha256"] != _digest(_json_bytes(payload)):
                raise ValueError("manifest checksum mismatch")
            if set(payload) != {"version", "identity", "complete", "responses"}:
                raise ValueError("invalid manifest fields")
            if payload["version"] != 1 or payload["complete"] is not True:
                raise ValueError("unsupported or incomplete capture")
            if _json_bytes(payload["identity"]) != _json_bytes(self.identity):
                raise ValueError("capture identity mismatch")
            responses = payload["responses"]
            if not isinstance(responses, dict):
                raise ValueError("invalid response index")
            for key, digest in responses.items():
                if not re.fullmatch(r"[0-9a-f]{64}", key) or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                    raise ValueError("invalid response checksum")
            self._responses = responses
            for key in responses:
                self._response(key)
        except (TypeError, ValueError, KeyError) as error:
            raise RuntimeError(f"invalid capture manifest: {error}") from error

    def _response(self, key):
        data, response = self._read_json(self.directory / f"{key}.json")
        try:
            if _digest(data) != self._responses[key]:
                raise ValueError("response checksum mismatch")
            if set(response) != {"request", "returncode", "stdout", "stderr"}:
                raise ValueError("invalid response fields")
            if _digest(_json_bytes(response["request"])) != key:
                raise ValueError("response request mismatch")
            if type(response["returncode"]) is not int or not isinstance(response["stdout"], str) or not isinstance(response["stderr"], str):
                raise ValueError("invalid subprocess response")
        except (TypeError, ValueError, KeyError) as error:
            raise RuntimeError(f"corrupt capture response: {key}") from error
        return response

    def _request(self, args, kwargs):
        allowed = {"input", "capture_output", "text", "timeout", "check",
                   "encoding", "errors", "cwd"}
        if set(kwargs) - allowed:
            raise ValueError("unsupported capture subprocess options")
        if kwargs.get("capture_output") is not True or kwargs.get("text") is not True or kwargs.get("check", False) is not False:
            raise ValueError("capture requires capture_output=True, text=True, check=False")
        if isinstance(args, (str, bytes, os.PathLike)):
            raise TypeError("capture args must be an argv sequence")
        argv = [os.fspath(arg) for arg in args]
        if not all(isinstance(arg, str) for arg in argv):
            raise TypeError("capture argv must contain text paths or strings")
        stdin = kwargs.get("input")
        if stdin is not None and not isinstance(stdin, str):
            raise TypeError("capture stdin must be text or None")
        encoding = kwargs.get("encoding") or ("utf-8" if sys.flags.utf8_mode else locale.getencoding())
        return {"argv": argv, "cwd": str(Path(kwargs.get("cwd") or Path.cwd()).resolve()),
                "input": stdin, "encoding": encoding,
                "errors": kwargs.get("errors") or "strict",
                "timeout": kwargs.get("timeout")}

    def run(self, args, **kwargs) -> subprocess.CompletedProcess:
        with self._lock:
            if self._finished or self._failed:
                raise RuntimeError("capture is already finished or failed")
            self._active += 1
        try:
            request = self._request(args, kwargs)
            key = _digest(_json_bytes(request))
            if self.replay:
                with self._lock:
                    if self._failed:
                        raise RuntimeError("capture has failed")
                    if key not in self._responses:
                        raise RuntimeError("request is absent from completed capture")
                    response = self._response(key)
                    if response["request"] != request:
                        raise RuntimeError("capture request mismatch")
                    self._used.add(key)
                    return subprocess.CompletedProcess(args, response["returncode"],
                                                       response["stdout"], response["stderr"])
            result = subprocess.run(args, **kwargs)
            response = {"request": request, "returncode": result.returncode,
                        "stdout": result.stdout, "stderr": result.stderr}
            if type(result.returncode) is not int or not isinstance(result.stdout, str) or not isinstance(result.stderr, str):
                raise RuntimeError("producer did not return a text subprocess response")
            data = _json_bytes(response)
            with self._lock:
                if self._failed:
                    raise RuntimeError("capture has failed")
                if key in self._responses:
                    if self._responses[key] != _digest(data):
                        raise RuntimeError("repeated producer request returned inconsistent responses")
                else:
                    # Each file is a durable checkpoint; index once to avoid quadratic rewrites.
                    self._responses[key] = self._atomic_json(self.directory / f"{key}.json", response)
            return result
        except BaseException:
            with self._lock:
                self._failed = True
            raise
        finally:
            with self._lock:
                self._active -= 1

    def finish(self) -> None:
        with self._lock:
            if self._failed:
                raise RuntimeError("cannot complete a failed capture")
            if self._finished:
                raise RuntimeError("capture is already finished")
            if self._active:
                raise RuntimeError("cannot complete capture while requests are running")
            if self.replay and self._used != self._responses.keys():
                raise RuntimeError("replay did not consume all captured requests "
                                   f"({len(self._responses) - len(self._used)} unused)")
            if not self.replay:
                try:
                    self._checkpoint(True)
                except BaseException:
                    self._failed = True
                    raise
            self._finished = True
