//! Static LaTeX math command classification and Unicode glyph canonicalization.

use unicode_normalization::UnicodeNormalization;

pub(crate) fn canonical(value: char) -> impl Iterator<Item = char> {
    // Normalize typographic/operator codepoint variants that share a TeX glyph.
    let mapped = match value {
        '−' => '-',
        '∗' => '*',
        '∙' => '•',
        '·' => '⋅',
        '⋯' => '…',
        '∆' => 'Δ',
        '⩽' => '≤',
        '⩾' => '≥',
        '∣' => '|',
        '∥' => '‖',
        '◻' => '□',
        '⟸' => '⇐',
        '⟹' => '⇒',
        '⟺' => '⇔',
        '⋃' => '∪',
        '⋂' => '∩',
        '⋁' => '∨',
        '⋀' => '∧',
        '⨀' => '⊙',
        '⨁' => '⊕',
        '⨂' => '⊗',
        '⨄' => '⊎',
        '⨆' => '⊔',
        _ => value,
    };
    std::iter::once(mapped).nfkc()
}

pub(crate) fn negated_symbol(name: &str) -> Option<char> {
    match name {
        "in" => Some('∉'),
        "owns" | "ni" => Some('∌'),
        "exists" => Some('∄'),
        "equiv" => Some('≢'),
        "le" | "leq" | "leqslant" => Some('≰'),
        "ge" | "geq" | "geqslant" => Some('≱'),
        "lt" => Some('≮'),
        "gt" => Some('≯'),
        "subset" => Some('⊄'),
        "supset" => Some('⊅'),
        "subseteq" => Some('⊈'),
        "supseteq" => Some('⊉'),
        "sim" => Some('≁'),
        "approx" => Some('≉'),
        "cong" => Some('≇'),
        "prec" => Some('⊀'),
        "succ" => Some('⊁'),
        "preceq" => Some('⋠'),
        "succeq" => Some('⋡'),
        "vdash" => Some('⊬'),
        "models" | "vDash" => Some('⊭'),
        "Vdash" => Some('⊮'),
        "mid" | "vert" => Some('∤'),
        "parallel" | "Vert" | "|" => Some('∦'),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathCommand {
    /// Formatting/style wrapper, font switch, spacing, or accent whose argument
    /// (if any) keeps its underlying character identity.
    Transparent,
    /// Delimiter sizing prefix (`\left`, `\right`, `\middle`) that also
    /// consumes a following `.` dummy delimiter.
    DelimiterPrefix,
    /// TeX dimension consumer (`\kern`, `\mkern`, `\mskip`, `\hskip`, `\vskip`).
    Dimension { allow_glue: bool },
    /// Command whose mandatory `{...}` argument (and optional/overlay args) are
    /// non-math metadata or invisible (`\phantom`, `\textcolor`, `\ref`, `\cite`, etc.).
    DiscardArg { emit_gap: bool, trailing_opt: bool },
    /// `\sqrt` with optional `[n]` root index.
    Sqrt,
    /// `\xrightarrow` / `\xleftarrow` with optional `[sub]` argument.
    ExtensibleArrow(char),
    /// Single-character mathematical symbol, letter, operator, or delimiter.
    Symbol(char),
    /// Multi-character symbol sequence (`\coloneqq`, `\idotsint`, etc.).
    MultiSymbol(&'static [char]),
    /// Named operator or function rendered as one or more words (`\sin`, `\lim inf`, etc.).
    Words(&'static str),
    /// Unmodeled command: consumes optional/overlay arguments and emits a `'\0'` gap atom.
    Unknown,
}

pub(crate) fn classify_command(name: &str) -> MathCommand {
    match name {
        // Font/style wrappers, switches, accents, fractions, binomials, and spacing no-ops
        "mathcal"
        | "mathbb"
        | "mathbbm"
        | "mathbbb"
        | "mathds"
        | "mathfrak"
        | "mathscr"
        | "mathrm"
        | "mathbf"
        | "mathbfit"
        | "mathsf"
        | "mathsfit"
        | "mathbfsfit"
        | "mathbfcal"
        | "mathbfscr"
        | "mathtt"
        | "mathit"
        | "mathnormal"
        | "mathbold"
        | "symrm"
        | "symit"
        | "symsf"
        | "symtt"
        | "symbb"
        | "symcal"
        | "symscr"
        | "symfrak"
        | "symnormal"
        | "symbf"
        | "symbfit"
        | "symbfsfit"
        | "symsfit"
        | "symbfcal"
        | "symbffrak"
        | "symbfscr"
        | "symbfsf"
        | "symbftt"
        | "text"
        | "textbf"
        | "textit"
        | "textrm"
        | "textsf"
        | "texttt"
        | "textup"
        | "textnormal"
        | "textsc"
        | "textsl"
        | "textmd"
        | "textlf"
        | "textulc"
        | "normalfont"
        | "rmfamily"
        | "sffamily"
        | "ttfamily"
        | "bfseries"
        | "mdseries"
        | "lfseries"
        | "itshape"
        | "slshape"
        | "scshape"
        | "upshape"
        | "rm"
        | "bf"
        | "it"
        | "sl"
        | "sf"
        | "tt"
        | "sc"
        | "cal"
        | "mit"
        | "oldstylenums"
        | "emph"
        | "bm"
        | "boldsymbol"
        | "boxed"
        | "displaystyle"
        | "textstyle"
        | "scriptstyle"
        | "scriptscriptstyle"
        | "limits"
        | "nolimits"
        | "mathrel"
        | "mathbin"
        | "mathord"
        | "mathop"
        | "mathopen"
        | "mathclose"
        | "mathpunct"
        | "mathinner"
        | "quad"
        | "qquad"
        | "enspace"
        | "thinspace"
        | "medspace"
        | "thickspace"
        | "negthinspace"
        | "negmedspace"
        | "negthickspace"
        | ","
        | ":"
        | ";"
        | "!"
        | ">"
        | "/"
        | " "
        | "\t"
        | "\n"
        | "\r"
        | "\\"
        | "dot"
        | "ddot"
        | "bar"
        | "hat"
        | "check"
        | "breve"
        | "acute"
        | "grave"
        | "tilde"
        | "vec"
        | "widetilde"
        | "widehat"
        | "overline"
        | "underline"
        | "overrightarrow"
        | "overleftarrow"
        | "frac"
        | "dfrac"
        | "tfrac"
        | "cfrac"
        | "binom"
        | "tbinom"
        | "dbinom"
        | "substack"
        | "mbox"
        | "hbox"
        | "mathclap"
        | "mathllap"
        | "mathrlap"
        | "operatorname"
        | "bigl"
        | "bigr"
        | "Bigl"
        | "Bigr"
        | "biggl"
        | "biggr"
        | "Biggl"
        | "Biggr"
        | "bigm"
        | "Bigm"
        | "biggm"
        | "Biggm"
        | "big"
        | "Big"
        | "bigg"
        | "Bigg" => MathCommand::Transparent,

        // Variable-size delimiter prefixes that can take a `.` null delimiter
        "left" | "right" | "middle" => MathCommand::DelimiterPrefix,

        // Dimension consumers
        "kern" | "mkern" => MathCommand::Dimension { allow_glue: false },
        "mskip" | "hskip" | "vskip" => MathCommand::Dimension { allow_glue: true },

        // Metadata / invisible argument consumers
        "textcolor" | "color" | "phantom" | "hphantom" | "vphantom" | "hspace" | "vspace" => {
            MathCommand::DiscardArg {
                emit_gap: false,
                trailing_opt: false,
            }
        }
        "raisebox" => MathCommand::DiscardArg {
            emit_gap: false,
            trailing_opt: true,
        },
        "ref" | "eqref" | "pageref" | "autoref" | "cref" | "Cref" | "cite" | "citep" | "citet"
        | "nocite" => MathCommand::DiscardArg {
            emit_gap: true,
            trailing_opt: false,
        },

        // Structural math commands with optional [...] arguments
        "sqrt" => MathCommand::Sqrt,
        "xrightarrow" => MathCommand::ExtensibleArrow('→'),
        "xleftarrow" => MathCommand::ExtensibleArrow('←'),

        // Named operators and functions
        "sin" | "cos" | "tan" | "csc" | "sec" | "cot" | "arcsin" | "arccos" | "arctan"
        | "sinh" | "cosh" | "tanh" | "coth" | "arcsinh" | "arccosh" | "arctanh" | "csch"
        | "sech" | "exp" | "ln" | "log" | "lg" | "erf" | "erfc" | "arg" | "ker" | "dim"
        | "det" | "Pr" | "gcd" | "deg" | "hom" | "mod" | "lim" | "min" | "max" | "inf"
        | "sup" => MathCommand::Words( match name {
            "sin" => "sin",
            "cos" => "cos",
            "tan" => "tan",
            "csc" => "csc",
            "sec" => "sec",
            "cot" => "cot",
            "arcsin" => "arcsin",
            "arccos" => "arccos",
            "arctan" => "arctan",
            "sinh" => "sinh",
            "cosh" => "cosh",
            "tanh" => "tanh",
            "coth" => "coth",
            "arcsinh" => "arcsinh",
            "arccosh" => "arccosh",
            "arctanh" => "arctanh",
            "csch" => "csch",
            "sech" => "sech",
            "exp" => "exp",
            "ln" => "ln",
            "log" => "log",
            "lg" => "lg",
            "erf" => "erf",
            "erfc" => "erfc",
            "arg" => "arg",
            "ker" => "ker",
            "dim" => "dim",
            "det" => "det",
            "Pr" => "Pr",
            "gcd" => "gcd",
            "deg" => "deg",
            "hom" => "hom",
            "mod" => "mod",
            "lim" => "lim",
            "min" => "min",
            "max" => "max",
            "inf" => "inf",
            "sup" => "sup",
            _ => unreachable!(),
        }),
        "liminf" => MathCommand::Words("lim inf"),
        "limsup" => MathCommand::Words("lim sup"),
        "argmin" => MathCommand::Words("arg min"),
        "argmax" => MathCommand::Words("arg max"),
        "injlim" => MathCommand::Words("inj lim"),
        "projlim" => MathCommand::Words("proj lim"),
        "varinjlim" | "varprojlim" | "varliminf" | "varlimsup" => MathCommand::Words("lim"),

        // Multi-character symbols
        "idotsint" => MathCommand::MultiSymbol(&['∫', '…', '∫']),
        "coloneqq" | "coloneq" | "defeq" | "assign" => MathCommand::MultiSymbol(&[':', '=']),
        "Coloneqq" => MathCommand::MultiSymbol(&[':', ':', '=']),
        "eqqcolon" | "eqcolon" => MathCommand::MultiSymbol(&['=', ':']),
        "Eqqcolon" => MathCommand::MultiSymbol(&['=', ':', ':']),

        // Greek letters (uppercase, lowercase, and variants)
        "Alpha" => MathCommand::Symbol('Α'),
        "alpha" => MathCommand::Symbol('α'),
        "Beta" => MathCommand::Symbol('Β'),
        "beta" => MathCommand::Symbol('β'),
        "Gamma" | "varGamma" => MathCommand::Symbol('Γ'),
        "gamma" => MathCommand::Symbol('γ'),
        "digamma" => MathCommand::Symbol('ϝ'),
        "Delta" | "varDelta" => MathCommand::Symbol('Δ'),
        "delta" => MathCommand::Symbol('δ'),
        "Epsilon" => MathCommand::Symbol('Ε'),
        "epsilon" => MathCommand::Symbol('ϵ'),
        "varepsilon" => MathCommand::Symbol('ε'),
        "backepsilon" => MathCommand::Symbol('϶'),
        "Zeta" => MathCommand::Symbol('Ζ'),
        "zeta" => MathCommand::Symbol('ζ'),
        "Eta" => MathCommand::Symbol('Η'),
        "eta" => MathCommand::Symbol('η'),
        "Theta" | "varTheta" => MathCommand::Symbol('Θ'),
        "theta" => MathCommand::Symbol('θ'),
        "vartheta" => MathCommand::Symbol('ϑ'),
        "Iota" => MathCommand::Symbol('Ι'),
        "iota" => MathCommand::Symbol('ι'),
        "Kappa" => MathCommand::Symbol('Κ'),
        "kappa" => MathCommand::Symbol('κ'),
        "varkappa" => MathCommand::Symbol('ϰ'),
        "Lambda" | "varLambda" => MathCommand::Symbol('Λ'),
        "lambda" => MathCommand::Symbol('λ'),
        "Mu" => MathCommand::Symbol('Μ'),
        "mu" => MathCommand::Symbol('μ'),
        "Nu" => MathCommand::Symbol('Ν'),
        "nu" => MathCommand::Symbol('ν'),
        "Xi" | "varXi" => MathCommand::Symbol('Ξ'),
        "xi" => MathCommand::Symbol('ξ'),
        "Omicron" => MathCommand::Symbol('Ο'),
        "omicron" => MathCommand::Symbol('ο'),
        "Pi" | "varPi" => MathCommand::Symbol('Π'),
        "pi" => MathCommand::Symbol('π'),
        "varpi" => MathCommand::Symbol('ϖ'),
        "Rho" => MathCommand::Symbol('Ρ'),
        "rho" => MathCommand::Symbol('ρ'),
        "varrho" => MathCommand::Symbol('ϱ'),
        "Sigma" | "varSigma" => MathCommand::Symbol('Σ'),
        "sigma" => MathCommand::Symbol('σ'),
        "varsigma" => MathCommand::Symbol('ς'),
        "Tau" => MathCommand::Symbol('Τ'),
        "tau" => MathCommand::Symbol('τ'),
        "Upsilon" | "varUpsilon" => MathCommand::Symbol('Υ'),
        "upsilon" => MathCommand::Symbol('υ'),
        "Phi" | "varPhi" => MathCommand::Symbol('Φ'),
        "phi" => MathCommand::Symbol('ϕ'),
        "varphi" => MathCommand::Symbol('φ'),
        "Chi" => MathCommand::Symbol('Χ'),
        "chi" => MathCommand::Symbol('χ'),
        "Psi" | "varPsi" => MathCommand::Symbol('Ψ'),
        "psi" => MathCommand::Symbol('ψ'),
        "Omega" | "varOmega" => MathCommand::Symbol('Ω'),
        "omega" => MathCommand::Symbol('ω'),

        // Hebrew, letter-like, and Latin ligature symbols
        "aleph" => MathCommand::Symbol('ℵ'),
        "beth" => MathCommand::Symbol('ℶ'),
        "gimel" => MathCommand::Symbol('ℷ'),
        "daleth" => MathCommand::Symbol('ℸ'),
        "imath" => MathCommand::Symbol('ı'),
        "jmath" => MathCommand::Symbol('ȷ'),
        "ell" => MathCommand::Symbol('ℓ'),
        "wp" => MathCommand::Symbol('℘'),
        "hbar" | "hslash" => MathCommand::Symbol('ℏ'),
        "infty" => MathCommand::Symbol('∞'),
        "mho" => MathCommand::Symbol('℧'),
        "Finv" => MathCommand::Symbol('Ⅎ'),
        "Game" => MathCommand::Symbol('⅁'),
        "Bbbk" => MathCommand::Symbol('k'),
        "Re" => MathCommand::Symbol('ℜ'),
        "Im" => MathCommand::Symbol('ℑ'),
        "complement" => MathCommand::Symbol('∁'),
        "emptyset" | "varnothing" => MathCommand::Symbol('∅'),
        "partial" => MathCommand::Symbol('∂'),
        "nabla" => MathCommand::Symbol('∇'),
        "forall" => MathCommand::Symbol('∀'),
        "exists" => MathCommand::Symbol('∃'),
        "nexists" => MathCommand::Symbol('∄'),
        "therefore" => MathCommand::Symbol('∴'),
        "because" => MathCommand::Symbol('∵'),
        "angle" => MathCommand::Symbol('∠'),
        "measuredangle" => MathCommand::Symbol('∡'),
        "sphericalangle" => MathCommand::Symbol('∢'),
        "surd" => MathCommand::Symbol('√'),
        "prime" => MathCommand::Symbol('′'),
        "backprime" => MathCommand::Symbol('‵'),
        "A" => MathCommand::Symbol('Å'),
        "a" => MathCommand::Symbol('å'),
        "AE" => MathCommand::Symbol('Æ'),
        "ae" => MathCommand::Symbol('æ'),
        "DH" => MathCommand::Symbol('Ð'),
        "dh" | "eth" => MathCommand::Symbol('ð'),
        "dj" => MathCommand::Symbol('đ'),
        "L" => MathCommand::Symbol('Ł'),
        "l" => MathCommand::Symbol('ł'),
        "NG" => MathCommand::Symbol('Ŋ'),
        "ng" => MathCommand::Symbol('ŋ'),
        "O" => MathCommand::Symbol('Ø'),
        "o" => MathCommand::Symbol('ø'),
        "OE" => MathCommand::Symbol('Œ'),
        "oe" => MathCommand::Symbol('œ'),
        "ss" => MathCommand::Symbol('ß'),
        "TH" => MathCommand::Symbol('Þ'),
        "th" => MathCommand::Symbol('þ'),

        // Escaped punctuation, currency, suits, and miscellaneous symbols
        "{" => MathCommand::Symbol('{'),
        "}" => MathCommand::Symbol('}'),
        "%" => MathCommand::Symbol('%'),
        "_" => MathCommand::Symbol('_'),
        "&" | "And" => MathCommand::Symbol('&'),
        "#" => MathCommand::Symbol('#'),
        "$" => MathCommand::Symbol('$'),
        "backslash" => MathCommand::Symbol('\\'),
        "colon" => MathCommand::Symbol(':'),
        "S" | "mathsection" => MathCommand::Symbol('§'),
        "P" | "mathparagraph" => MathCommand::Symbol('¶'),
        "copyright" => MathCommand::Symbol('©'),
        "checkmark" => MathCommand::Symbol('✓'),
        "circledR" => MathCommand::Symbol('Ⓡ'),
        "maltese" => MathCommand::Symbol('✠'),
        "dagger" | "dag" => MathCommand::Symbol('†'),
        "ddagger" | "Dagger" | "ddag" => MathCommand::Symbol('‡'),
        "pounds" | "mathsterling" => MathCommand::Symbol('£'),
        "yen" | "textyen" => MathCommand::Symbol('¥'),
        "euro" => MathCommand::Symbol('€'),
        "rupee" => MathCommand::Symbol('₹'),
        "flat" => MathCommand::Symbol('♭'),
        "natural" => MathCommand::Symbol('♮'),
        "sharp" => MathCommand::Symbol('♯'),
        "diamondsuit" => MathCommand::Symbol('♢'),
        "heartsuit" => MathCommand::Symbol('♡'),
        "clubsuit" => MathCommand::Symbol('♣'),
        "spadesuit" => MathCommand::Symbol('♠'),
        "sun" => MathCommand::Symbol('☼'),
        "mercury" => MathCommand::Symbol('☿'),
        "venus" => MathCommand::Symbol('♀'),
        "earth" => MathCommand::Symbol('♁'),
        "mars" => MathCommand::Symbol('♂'),
        "jupiter" => MathCommand::Symbol('♃'),
        "saturn" => MathCommand::Symbol('♄'),
        "uranus" => MathCommand::Symbol('♅'),
        "neptune" => MathCommand::Symbol('♆'),
        "astrosun" => MathCommand::Symbol('☉'),
        "ascnode" => MathCommand::Symbol('☊'),

        // Delimiters and corners
        "langle" => MathCommand::Symbol('⟨'),
        "rangle" => MathCommand::Symbol('⟩'),
        "lceil" => MathCommand::Symbol('⌈'),
        "rceil" => MathCommand::Symbol('⌉'),
        "lfloor" => MathCommand::Symbol('⌊'),
        "rfloor" => MathCommand::Symbol('⌋'),
        "lgroup" => MathCommand::Symbol('⦗'),
        "rgroup" => MathCommand::Symbol('⦘'),
        "llbracket" => MathCommand::Symbol('⟦'),
        "rrbracket" => MathCommand::Symbol('⟧'),
        "ulcorner" => MathCommand::Symbol('⌜'),
        "urcorner" => MathCommand::Symbol('⌝'),
        "llcorner" => MathCommand::Symbol('⌞'),
        "lrcorner" => MathCommand::Symbol('⌟'),
        "lvert" | "rvert" | "vert" | "mid" => MathCommand::Symbol('|'),
        "lVert" | "rVert" | "Vert" | "|" | "parallel" => MathCommand::Symbol('‖'),
        "nmid" => MathCommand::Symbol('∤'),
        "nparallel" => MathCommand::Symbol('∦'),

        // Big operators and integrals
        "sum" => MathCommand::Symbol('∑'),
        "prod" => MathCommand::Symbol('∏'),
        "coprod" => MathCommand::Symbol('∐'),
        "bigcap" => MathCommand::Symbol('⋂'),
        "bigcup" => MathCommand::Symbol('⋃'),
        "bigsqcup" | "sqcup" => MathCommand::Symbol('⊔'),
        "bigsqcap" | "sqcap" => MathCommand::Symbol('⊓'),
        "bigvee" => MathCommand::Symbol('⋁'),
        "bigwedge" => MathCommand::Symbol('⋀'),
        "bigodot" => MathCommand::Symbol('⨀'),
        "bigotimes" => MathCommand::Symbol('⨂'),
        "bigoplus" => MathCommand::Symbol('⨁'),
        "biguplus" => MathCommand::Symbol('⨄'),
        "int" | "intop" => MathCommand::Symbol('∫'),
        "iint" => MathCommand::Symbol('∬'),
        "iiint" => MathCommand::Symbol('∭'),
        "iiiint" => MathCommand::Symbol('⨌'),
        "oint" | "ointop" => MathCommand::Symbol('∮'),
        "oiint" => MathCommand::Symbol('∯'),
        "oiiint" => MathCommand::Symbol('∰'),

        // Binary operators, geometric shapes, and dots
        "times" => MathCommand::Symbol('×'),
        "div" => MathCommand::Symbol('÷'),
        "pm" => MathCommand::Symbol('±'),
        "mp" => MathCommand::Symbol('∓'),
        "cdot" | "centerdot" => MathCommand::Symbol('⋅'),
        "cdots" | "ldots" | "dots" | "dotsb" | "dotsc" | "dotsi" | "dotsm" | "dotso"
        | "hdots" => MathCommand::Symbol('…'),
        "vdots" => MathCommand::Symbol('⋮'),
        "ddots" => MathCommand::Symbol('⋱'),
        "circ" => MathCommand::Symbol('∘'),
        "bullet" => MathCommand::Symbol('•'),
        "ast" => MathCommand::Symbol('∗'),
        "star" => MathCommand::Symbol('⋆'),
        "bigstar" => MathCommand::Symbol('★'),
        "oplus" => MathCommand::Symbol('⊕'),
        "ominus" => MathCommand::Symbol('⊖'),
        "otimes" => MathCommand::Symbol('⊗'),
        "oslash" => MathCommand::Symbol('⊘'),
        "odot" => MathCommand::Symbol('⊙'),
        "bigcirc" => MathCommand::Symbol('◯'),
        "circledast" => MathCommand::Symbol('⊛'),
        "circledcirc" => MathCommand::Symbol('⊚'),
        "circleddash" => MathCommand::Symbol('⊝'),
        "boxdot" => MathCommand::Symbol('⊡'),
        "boxplus" => MathCommand::Symbol('⊞'),
        "boxminus" => MathCommand::Symbol('⊟'),
        "boxtimes" => MathCommand::Symbol('⊠'),
        "boxbox" => MathCommand::Symbol('⧈'),
        "boxslash" => MathCommand::Symbol('⧄'),
        "boxbslash" => MathCommand::Symbol('⧅'),
        "diamond" => MathCommand::Symbol('⋄'),
        "Diamond" | "lozenge" => MathCommand::Symbol('◊'),
        "blacklozenge" => MathCommand::Symbol('⧫'),
        "Box" | "square" => MathCommand::Symbol('□'),
        "blacksquare" => MathCommand::Symbol('■'),
        "triangle" | "vartriangle" | "bigtriangleup" => MathCommand::Symbol('△'),
        "bigtriangledown" | "triangledown" => MathCommand::Symbol('▽'),
        "triangleleft" => MathCommand::Symbol('◁'),
        "triangleright" => MathCommand::Symbol('▷'),
        "blacktriangle" => MathCommand::Symbol('▲'),
        "blacktriangledown" => MathCommand::Symbol('▼'),
        "blacktriangleleft" => MathCommand::Symbol('◀'),
        "blacktriangleright" => MathCommand::Symbol('▶'),
        "lhd" | "vartriangleleft" => MathCommand::Symbol('⊲'),
        "rhd" | "vartriangleright" => MathCommand::Symbol('⊳'),
        "unlhd" | "trianglelefteq" => MathCommand::Symbol('⊴'),
        "unrhd" | "trianglerighteq" => MathCommand::Symbol('⊵'),
        "amalg" => MathCommand::Symbol('⨿'),
        "wr" => MathCommand::Symbol('≀'),
        "intercal" => MathCommand::Symbol('⊺'),
        "dotplus" => MathCommand::Symbol('∔'),
        "divideontimes" => MathCommand::Symbol('⋇'),
        "ltimes" => MathCommand::Symbol('⋉'),
        "rtimes" => MathCommand::Symbol('⋊'),
        "leftthreetimes" => MathCommand::Symbol('⋋'),
        "rightthreetimes" => MathCommand::Symbol('⋌'),
        "bowtie" | "Join" => MathCommand::Symbol('⋈'),
        "cap" => MathCommand::Symbol('∩'),
        "cup" => MathCommand::Symbol('∪'),
        "Cap" => MathCommand::Symbol('⋒'),
        "Cup" => MathCommand::Symbol('⋓'),
        "uplus" => MathCommand::Symbol('⊎'),
        "setminus" | "smallsetminus" => MathCommand::Symbol('∖'),
        "vee" | "lor" => MathCommand::Symbol('∨'),
        "wedge" | "land" => MathCommand::Symbol('∧'),
        "lnot" | "neg" => MathCommand::Symbol('¬'),
        "barwedge" => MathCommand::Symbol('⊼'),
        "veebar" => MathCommand::Symbol('⊻'),
        "curlyvee" => MathCommand::Symbol('⋎'),
        "curlywedge" => MathCommand::Symbol('⋏'),

        // Relations, ordering, sets, and logic
        "le" | "leq" | "leqslant" => MathCommand::Symbol('≤'),
        "ge" | "geq" | "geqslant" => MathCommand::Symbol('≥'),
        "lt" => MathCommand::Symbol('<'),
        "gt" => MathCommand::Symbol('>'),
        "ll" => MathCommand::Symbol('≪'),
        "gg" => MathCommand::Symbol('≫'),
        "lll" => MathCommand::Symbol('⋘'),
        "ggg" => MathCommand::Symbol('⋙'),
        "leqq" => MathCommand::Symbol('≦'),
        "geqq" => MathCommand::Symbol('≧'),
        "eqslantless" => MathCommand::Symbol('⪕'),
        "eqslantgtr" => MathCommand::Symbol('⪖'),
        "lesssim" => MathCommand::Symbol('≲'),
        "gtrsim" => MathCommand::Symbol('≳'),
        "lessapprox" => MathCommand::Symbol('⪅'),
        "gtrapprox" => MathCommand::Symbol('⪆'),
        "lessdot" => MathCommand::Symbol('⋖'),
        "gtrdot" => MathCommand::Symbol('⋗'),
        "lessgtr" => MathCommand::Symbol('≶'),
        "gtrless" => MathCommand::Symbol('≷'),
        "lesseqgtr" => MathCommand::Symbol('⋚'),
        "gtreqless" => MathCommand::Symbol('⋛'),
        "lesseqqgtr" => MathCommand::Symbol('⪋'),
        "gtreqqless" => MathCommand::Symbol('⪌'),
        "nless" => MathCommand::Symbol('≮'),
        "ngtr" => MathCommand::Symbol('≯'),
        "nleq" | "nleqslant" => MathCommand::Symbol('≰'),
        "ngeq" | "ngeqslant" => MathCommand::Symbol('≱'),
        "prec" => MathCommand::Symbol('≺'),
        "succ" => MathCommand::Symbol('≻'),
        "preceq" => MathCommand::Symbol('⪯'),
        "succeq" => MathCommand::Symbol('⪰'),
        "preccurlyeq" => MathCommand::Symbol('≼'),
        "succcurlyeq" => MathCommand::Symbol('≽'),
        "precsim" => MathCommand::Symbol('≾'),
        "succsim" => MathCommand::Symbol('≿'),
        "nprec" => MathCommand::Symbol('⊀'),
        "nsucc" => MathCommand::Symbol('⊁'),
        "ne" | "neq" => MathCommand::Symbol('≠'),
        "equiv" => MathCommand::Symbol('≡'),
        "nequiv" => MathCommand::Symbol('≢'),
        "sim" | "thicksim" => MathCommand::Symbol('∼'),
        "nsim" => MathCommand::Symbol('≁'),
        "simeq" => MathCommand::Symbol('≃'),
        "eqsim" => MathCommand::Symbol('≂'),
        "backsimeq" => MathCommand::Symbol('⋍'),
        "approx" | "thickapprox" => MathCommand::Symbol('≈'),
        "approxeq" => MathCommand::Symbol('≊'),
        "cong" => MathCommand::Symbol('≅'),
        "ncong" => MathCommand::Symbol('≇'),
        "asymp" => MathCommand::Symbol('≍'),
        "propto" => MathCommand::Symbol('∝'),
        "doteq" => MathCommand::Symbol('≐'),
        "doteqdot" => MathCommand::Symbol('≑'),
        "risingdotseq" => MathCommand::Symbol('≓'),
        "fallingdotseq" => MathCommand::Symbol('≒'),
        "circeq" => MathCommand::Symbol('≗'),
        "triangleq" => MathCommand::Symbol('≜'),
        "bumpeq" => MathCommand::Symbol('≏'),
        "Bumpeq" => MathCommand::Symbol('≎'),
        "smile" | "smallsmile" => MathCommand::Symbol('⌣'),
        "frown" | "smallfrown" => MathCommand::Symbol('⌢'),
        "in" => MathCommand::Symbol('∈'),
        "ni" | "owns" => MathCommand::Symbol('∋'),
        "notin" => MathCommand::Symbol('∉'),
        "subset" => MathCommand::Symbol('⊂'),
        "supset" => MathCommand::Symbol('⊃'),
        "subseteq" => MathCommand::Symbol('⊆'),
        "supseteq" => MathCommand::Symbol('⊇'),
        "subseteqq" => MathCommand::Symbol('⫅'),
        "supseteqq" => MathCommand::Symbol('⫆'),
        "nsubseteq" => MathCommand::Symbol('⊈'),
        "nsupseteq" => MathCommand::Symbol('⊉'),
        "subsetneq" | "varsubsetneq" => MathCommand::Symbol('⊊'),
        "supsetneq" | "varsupsetneq" => MathCommand::Symbol('⊋'),
        "sqsubset" => MathCommand::Symbol('⊏'),
        "sqsupset" => MathCommand::Symbol('⊐'),
        "sqsubseteq" => MathCommand::Symbol('⊑'),
        "sqsupseteq" => MathCommand::Symbol('⊒'),
        "perp" | "bot" => MathCommand::Symbol('⊥'),
        "top" => MathCommand::Symbol('⊤'),
        "vdash" => MathCommand::Symbol('⊢'),
        "dashv" => MathCommand::Symbol('⊣'),
        "vDash" | "models" => MathCommand::Symbol('⊨'),
        "Vdash" => MathCommand::Symbol('⊩'),
        "Vvdash" => MathCommand::Symbol('⊪'),
        "nvdash" => MathCommand::Symbol('⊬'),
        "nvDash" => MathCommand::Symbol('⊭'),
        "nVdash" => MathCommand::Symbol('⊮'),
        "nVDash" => MathCommand::Symbol('⊯'),

        // Arrows and harpoons
        "leftarrow" | "gets" => MathCommand::Symbol('←'),
        "rightarrow" | "to" => MathCommand::Symbol('→'),
        "nleftarrow" => MathCommand::Symbol('↚'),
        "nrightarrow" => MathCommand::Symbol('↛'),
        "leftrightarrow" => MathCommand::Symbol('↔'),
        "nleftrightarrow" => MathCommand::Symbol('↮'),
        "longleftarrow" => MathCommand::Symbol('⟵'),
        "longrightarrow" => MathCommand::Symbol('⟶'),
        "longleftrightarrow" => MathCommand::Symbol('⟷'),
        "Leftarrow" | "Longleftarrow" | "impliedby" => MathCommand::Symbol('⇐'),
        "Rightarrow" | "Longrightarrow" | "implies" => MathCommand::Symbol('⇒'),
        "nLeftarrow" => MathCommand::Symbol('⇍'),
        "nRightarrow" => MathCommand::Symbol('⇏'),
        "Leftrightarrow" | "Longleftrightarrow" | "iff" => MathCommand::Symbol('⇔'),
        "nLeftrightarrow" => MathCommand::Symbol('⇎'),
        "uparrow" => MathCommand::Symbol('↑'),
        "downarrow" => MathCommand::Symbol('↓'),
        "updownarrow" => MathCommand::Symbol('↕'),
        "Uparrow" => MathCommand::Symbol('⇑'),
        "Downarrow" => MathCommand::Symbol('⇓'),
        "Updownarrow" => MathCommand::Symbol('⇕'),
        "nearrow" => MathCommand::Symbol('↗'),
        "searrow" => MathCommand::Symbol('↘'),
        "swarrow" => MathCommand::Symbol('↙'),
        "nwarrow" => MathCommand::Symbol('↖'),
        "rightharpoonup" => MathCommand::Symbol('⇀'),
        "rightharpoondown" => MathCommand::Symbol('⇁'),
        "leftharpoonup" => MathCommand::Symbol('↼'),
        "leftharpoondown" => MathCommand::Symbol('↽'),
        "upharpoonleft" => MathCommand::Symbol('↿'),
        "upharpoonright" => MathCommand::Symbol('↾'),
        "downharpoonleft" => MathCommand::Symbol('⇃'),
        "downharpoonright" => MathCommand::Symbol('⇂'),
        "rightleftharpoons" => MathCommand::Symbol('⇌'),
        "leftrightharpoons" => MathCommand::Symbol('⇋'),
        "curvearrowleft" => MathCommand::Symbol('↶'),
        "curvearrowright" => MathCommand::Symbol('↷'),
        "circlearrowleft" => MathCommand::Symbol('↺'),
        "circlearrowright" => MathCommand::Symbol('↻'),
        "Lsh" => MathCommand::Symbol('↰'),
        "Rsh" => MathCommand::Symbol('↱'),
        "upuparrows" => MathCommand::Symbol('⇈'),
        "downdownarrows" => MathCommand::Symbol('⇊'),
        "rightrightarrows" => MathCommand::Symbol('⇉'),
        "leftleftarrows" => MathCommand::Symbol('⇇'),
        "rightleftarrows" => MathCommand::Symbol('⇄'),
        "leftrightarrows" => MathCommand::Symbol('⇆'),
        "Lleftarrow" => MathCommand::Symbol('⇚'),
        "Rrightarrow" => MathCommand::Symbol('⇛'),
        "leftarrowtail" => MathCommand::Symbol('↢'),
        "rightarrowtail" => MathCommand::Symbol('↣'),
        "looparrowleft" => MathCommand::Symbol('↫'),
        "looparrowright" => MathCommand::Symbol('↬'),
        "mapsto" => MathCommand::Symbol('↦'),
        "longmapsto" => MathCommand::Symbol('⟼'),
        "mapsfrom" => MathCommand::Symbol('↤'),
        "longmapsfrom" => MathCommand::Symbol('⟻'),
        "Longmapsto" => MathCommand::Symbol('⟾'),
        "hookrightarrow" => MathCommand::Symbol('↪'),
        "hookleftarrow" => MathCommand::Symbol('↩'),
        "twoheadrightarrow" => MathCommand::Symbol('↠'),
        "twoheadleftarrow" => MathCommand::Symbol('↞'),
        "multimap" => MathCommand::Symbol('⊸'),
        "leftrightsquigarrow" => MathCommand::Symbol('↭'),
        "rightsquigarrow" | "leadsto" => MathCommand::Symbol('⇝'),
        "lightning" => MathCommand::Symbol('↯'),
        "Yleft" => MathCommand::Symbol('⤙'),
        "Yright" => MathCommand::Symbol('⤚'),

        _ => MathCommand::Unknown,
    }
}
