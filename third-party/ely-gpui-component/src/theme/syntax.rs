use gpui::rgb;

use super::{Mode, Palette, Syntax};

/// 代码高亮主题；浅色和深色模式分别由 `CodeRenderSettings` 选择。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeSyntaxTheme {
    /// GitHub Primer 的浅色 token 颜色。
    #[default]
    GitHubLight,
    /// GitHub Primer 的深色 token 颜色。
    GitHubDark,
    Ely,
    Quiet,
    Paper,
}

impl CodeSyntaxTheme {
    pub fn name(self) -> &'static str {
        match self {
            Self::GitHubLight => "GitHub Light",
            Self::GitHubDark => "GitHub Dark",
            Self::Ely => "Ely",
            Self::Quiet => "Quiet",
            Self::Paper => "Paper",
        }
    }

    pub fn syntax(self, mode: Mode) -> Syntax {
        syntax_themes()[self.index()].of(mode)
    }

    const fn index(self) -> usize {
        match self {
            Self::GitHubLight => 0,
            Self::GitHubDark => 1,
            Self::Ely => 2,
            Self::Quiet => 3,
            Self::Paper => 4,
        }
    }
}

/// A code palette by name, one for each mode.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxTheme {
    pub name: &'static str,
    pub light: Syntax,
    pub dark: Syntax,
}

impl SyntaxTheme {
    pub fn of(&self, mode: Mode) -> Syntax {
        match mode {
            Mode::Light => self.light.clone(),
            Mode::Dark => self.dark.clone(),
        }
    }
}

/// Thirteen colors in `Syntax`'s field order: keyword, string, number, comment, function, type, constant, property, tag, attribute, operator, punctuation, variable.
fn syntax(hex: [u32; 13]) -> Syntax {
    let [
        keyword,
        string,
        number,
        comment,
        function,
        type_name,
        constant,
        property,
        tag,
        attribute,
        operator,
        punctuation,
        variable,
    ] = hex.map(|value| rgb(value).into());
    Syntax {
        keyword,
        string,
        number,
        comment,
        function,
        type_name,
        constant,
        property,
        tag,
        attribute,
        operator,
        punctuation,
        variable,
    }
}

/// GitHub's token colors, Ely's code colors, a quiet set, and a warm paper set.
pub fn syntax_themes() -> [SyntaxTheme; 5] {
    [
        SyntaxTheme {
            name: "GitHub Light",
            light: syntax([
                0xcf222e, 0x0a3069, 0x0550ae, 0x6e7781, 0x8250df, 0x953800, 0x0550ae, 0x0550ae,
                0x116329, 0x953800, 0xcf222e, 0x24292f, 0x24292f,
            ]),
            dark: syntax([
                0xff7b72, 0xa5d6ff, 0x79c0ff, 0x8b949e, 0xd2a8ff, 0xffa657, 0x79c0ff, 0x79c0ff,
                0x7ee787, 0x79c0ff, 0xff7b72, 0xc9d1d9, 0xffa657,
            ]),
        },
        SyntaxTheme {
            name: "GitHub Dark",
            light: syntax([
                0x8b949e, 0x0a3069, 0x0550ae, 0x6e7781, 0x8250df, 0x953800, 0x0550ae, 0x0550ae,
                0x116329, 0x953800, 0x8b949e, 0x24292f, 0x24292f,
            ]),
            dark: syntax([
                0xff7b72, 0xa5d6ff, 0x79c0ff, 0x8b949e, 0xd2a8ff, 0xffa657, 0x79c0ff, 0x79c0ff,
                0x7ee787, 0x79c0ff, 0xff7b72, 0xc9d1d9, 0xffa657,
            ]),
        },
        SyntaxTheme {
            name: "Ely",
            light: Palette::light(false).syntax,
            dark: Palette::dark(false).syntax,
        },
        SyntaxTheme {
            name: "Quiet",
            light: syntax([
                0x3d3a36, 0x5b6f4a, 0x7a5a3a, 0x9a9793, 0x2f2c29, 0x4f5b66, 0x7a5a3a, 0x5a5550,
                0x4f5b66, 0x7a5a3a, 0x6b6865, 0x8a8784, 0x181613,
            ]),
            dark: syntax([
                0xd6d3d0, 0xa8b894, 0xd0b48f, 0x6e6b68, 0xf3f1f0, 0xa9b6c2, 0xd0b48f, 0xc2beba,
                0xa9b6c2, 0xd0b48f, 0x9b9895, 0x7f7c79, 0xf3f1f0,
            ]),
        },
        SyntaxTheme {
            name: "Paper",
            light: syntax([
                0x8a4f2b, 0x5f7a3a, 0x9a6a1f, 0xa39a8c, 0x3d5a80, 0x2f6f6a, 0x9a6a1f, 0x7a4b5c,
                0x7a4b5c, 0x8a4f2b, 0x6f6a62, 0x8f887e, 0x2a2622,
            ]),
            dark: syntax([
                0xd69a72, 0xa9c287, 0xe0b56e, 0x7d766b, 0x92b4d8, 0x7fc2ba, 0xe0b56e, 0xd49aae,
                0xd49aae, 0xd69a72, 0xa8a298, 0x8a8479, 0xece6dc,
            ]),
        },
    ]
}
