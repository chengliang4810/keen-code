use std::fmt;

use gpui::{
    AnyElement, App, IntoElement, ParentElement, Rems, RenderOnce, Styled, Window, div, prelude::*,
};

use crate::theme::{ActiveTheme, TextSize};

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Var(char),
    Sym(String),
    Op(String),
    Word(String),
    Row(Vec<Node>),
    Script {
        base: Box<Node>,
        sup: Option<Box<Node>>,
        sub: Option<Box<Node>>,
    },
    Frac(Box<Node>, Box<Node>),
    Sqrt(Box<Node>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum MathError {
    Unknown(String),
    Unbalanced,
    MissingArgument(&'static str),
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MathError::Unknown(name) => write!(f, "unknown command \\{name}"),
            MathError::Unbalanced => write!(f, "unbalanced braces"),
            MathError::MissingArgument(name) => write!(f, "\\{name} is missing an argument"),
        }
    }
}

impl std::error::Error for MathError {}

fn symbol(name: &str) -> Option<Node> {
    let greek = "alpha α beta β gamma γ delta δ epsilon ε zeta ζ eta η theta θ iota ι kappa κ \
                 lambda λ mu μ nu ν xi ξ pi π rho ρ sigma σ tau τ upsilon υ phi φ chi χ psi ψ omega ω";
    let upper = "Gamma Γ Delta Δ Theta Θ Lambda Λ Xi Ξ Pi Π Sigma Σ Phi Φ Psi Ψ Omega Ω";
    let ops = "pm ± mp ∓ times × cdot · div ÷ leq ≤ geq ≥ neq ≠ approx ≈ equiv ≡ to → \
               rightarrow → leftarrow ← in ∈ subset ⊂ cup ∪ cap ∩";
    let plain = "infty ∞ partial ∂ nabla ∇ sum ∑ prod ∏ int ∫ forall ∀ exists ∃ ldots … cdots ⋯";
    let find = |table: &str| {
        let words: Vec<&str> = table.split_whitespace().collect();
        words
            .chunks(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].to_string())
    };
    if let Some(glyph) = find(greek) {
        return glyph.chars().next().map(Node::Var);
    }
    if let Some(glyph) = find(ops) {
        return Some(Node::Op(glyph));
    }
    find(upper).or_else(|| find(plain)).map(Node::Sym)
}

const FUNCTIONS: [&str; 11] = [
    "sin", "cos", "tan", "log", "ln", "exp", "lim", "max", "min", "det", "gcd",
];

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let next = self.peek();
        self.at += 1;
        next
    }

    fn row(&mut self, closed: bool) -> Result<Vec<Node>, MathError> {
        let mut row = Vec::new();
        loop {
            match self.peek() {
                None if closed => return Err(MathError::Unbalanced),
                None => return Ok(row),
                Some('}') if closed => {
                    self.bump();
                    return Ok(row);
                }
                Some('}') => return Err(MathError::Unbalanced),
                Some(ch) if ch.is_whitespace() => {
                    self.bump();
                }
                Some(mark @ ('^' | '_')) => {
                    self.bump();
                    let script = Box::new(self.argument(if mark == '^' { "^" } else { "_" })?);
                    let base = row.pop().unwrap_or(Node::Sym(String::new()));
                    row.push(attach(base, mark == '^', script));
                }
                Some(_) => {
                    let atom = self.atom()?;
                    row.push(atom);
                }
            }
        }
    }

    fn atom(&mut self) -> Result<Node, MathError> {
        match self.bump() {
            Some('{') => Ok(Node::Row(self.row(true)?)),
            Some('\\') => self.command(),
            Some('-') => Ok(Node::Op("−".into())),
            Some(ch @ ('+' | '=' | '<' | '>')) => Ok(Node::Op(ch.to_string())),
            Some(ch) if ch.is_alphabetic() => Ok(Node::Var(ch)),
            Some(ch) => Ok(Node::Sym(ch.to_string())),
            None => Err(MathError::Unbalanced),
        }
    }

    fn argument(&mut self, name: &'static str) -> Result<Node, MathError> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        match self.peek() {
            None | Some('}') => Err(MathError::MissingArgument(name)),
            Some(_) => self.atom(),
        }
    }

    /// `{...}` read as plain text, spaces kept.
    fn raw_group(&mut self) -> Result<String, MathError> {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        if self.bump() != Some('{') {
            return Err(MathError::MissingArgument("text"));
        }
        let mut depth = 1;
        let mut text = String::new();
        loop {
            match self.bump().ok_or(MathError::Unbalanced)? {
                '{' => depth += 1,
                '}' if depth == 1 => return Ok(text),
                '}' => depth -= 1,
                _ => {}
            }
            text.push(self.chars[self.at - 1]);
        }
    }

    fn command(&mut self) -> Result<Node, MathError> {
        let start = self.at;
        while self.peek().is_some_and(|ch| ch.is_ascii_alphabetic()) {
            self.bump();
        }
        let name: String = self.chars[start..self.at].iter().collect();
        if name.is_empty() {
            let escaped = self.bump().ok_or(MathError::Unbalanced)?;
            return Ok(Node::Sym(if escaped == ',' {
                " ".into()
            } else {
                escaped.to_string()
            }));
        }
        match name.as_str() {
            "frac" => {
                let top = self.argument("frac")?;
                let bottom = self.argument("frac")?;
                Ok(Node::Frac(Box::new(top), Box::new(bottom)))
            }
            "sqrt" => Ok(Node::Sqrt(Box::new(self.argument("sqrt")?))),
            "text" => Ok(Node::Word(self.raw_group()?)),
            "left" | "right" => self.argument("left"),
            name if FUNCTIONS.contains(&name) => Ok(Node::Word(name.to_string())),
            name => symbol(name).ok_or_else(|| MathError::Unknown(name.to_string())),
        }
    }
}

fn attach(base: Node, upper: bool, script: Box<Node>) -> Node {
    match base {
        Node::Script { base, sup, sub } if upper && sup.is_none() => Node::Script {
            base,
            sup: Some(script),
            sub,
        },
        Node::Script { base, sup, sub } if !upper && sub.is_none() => Node::Script {
            base,
            sup,
            sub: Some(script),
        },
        base => {
            let (sup, sub) = if upper {
                (Some(script), None)
            } else {
                (None, Some(script))
            };
            Node::Script {
                base: Box::new(base),
                sup,
                sub,
            }
        }
    }
}

fn draw(node: &Node, size: Rems, cx: &App) -> AnyElement {
    let rule = cx.theme().colors.fg;
    let script = size * 0.72;
    let text = |content: String| div().text_size(size).child(content);
    match node {
        Node::Var(ch) => text(ch.to_string()).italic().into_any_element(),
        Node::Sym(content) | Node::Word(content) => text(content.clone()).into_any_element(),
        Node::Op(content) => text(content.clone()).px(size * 0.22).into_any_element(),
        Node::Row(nodes) => div()
            .flex()
            .items_center()
            .children(nodes.iter().map(|node| draw(node, size, cx)))
            .into_any_element(),
        Node::Script { base, sup, sub } => div()
            .flex()
            .items_center()
            .child(draw(base, size, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when(sub.is_none(), |col| col.mb(size * 0.55))
                    .when(sup.is_none(), |col| col.mt(size * 0.45))
                    .children(sup.iter().map(|node| draw(node, script, cx)))
                    .children(sub.iter().map(|node| draw(node, script, cx))),
            )
            .into_any_element(),
        Node::Frac(top, bottom) => div()
            .flex()
            .flex_col()
            .items_center()
            .px(size * 0.12)
            .child(draw(top, size * 0.85, cx))
            .child(div().w_full().border_t_1().border_color(rule))
            .child(draw(bottom, size * 0.85, cx))
            .into_any_element(),
        Node::Sqrt(body) => div()
            .flex()
            .items_center()
            .child(text("√".into()))
            .child(
                div()
                    .border_t_1()
                    .border_color(rule)
                    .child(draw(body, size, cx)),
            )
            .into_any_element(),
    }
}

/// Inline math from a TeX subset: scripts, `\frac`, `\sqrt`, Greek, operators.
#[derive(IntoElement)]
pub struct Latex {
    root: Node,
    size: TextSize,
}

impl Latex {
    pub fn parse(source: &str) -> Result<Self, MathError> {
        let mut parser = Parser {
            chars: source.chars().collect(),
            at: 0,
        };
        Ok(Self {
            root: Node::Row(parser.row(false)?),
            size: TextSize::Lg,
        })
    }

    pub fn size(mut self, size: TextSize) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Latex {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let size = theme.text_size(self.size);
        div()
            .text_color(theme.colors.fg)
            .child(draw(&self.root, size, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Result<Node, MathError> {
        Latex::parse(source).map(|latex| latex.root)
    }

    #[test]
    fn scripts_attach_to_the_previous_atom() {
        let Node::Row(nodes) = parse("x_i^2 + y").unwrap() else {
            panic!("row")
        };
        assert_eq!(nodes.len(), 3);
        let Node::Script { base, sup, sub } = &nodes[0] else {
            panic!("script")
        };
        assert_eq!(**base, Node::Var('x'));
        assert_eq!(sup.as_deref(), Some(&Node::Sym("2".into())));
        assert_eq!(sub.as_deref(), Some(&Node::Var('i')));
        assert_eq!(nodes[1], Node::Op("+".into()));
    }

    #[test]
    fn commands_build_structure() {
        let Node::Row(nodes) = parse(r"\frac{\alpha}{2} \sqrt{x} \sin").unwrap() else {
            panic!("row")
        };
        assert!(
            matches!(&nodes[0], Node::Frac(top, _) if **top == Node::Row(vec![Node::Var('α')]))
        );
        assert!(matches!(&nodes[1], Node::Sqrt(_)));
        assert_eq!(nodes[2], Node::Word("sin".into()));
    }

    #[test]
    fn text_keeps_its_spaces() {
        let Node::Row(nodes) = parse(r"\text{hello world} x").unwrap() else {
            panic!("row")
        };
        assert_eq!(nodes[0], Node::Word("hello world".into()));
    }

    #[test]
    fn errors_name_the_fault() {
        assert_eq!(parse(r"\foo"), Err(MathError::Unknown("foo".into())));
        assert_eq!(parse("{x"), Err(MathError::Unbalanced));
        assert_eq!(parse("x}"), Err(MathError::Unbalanced));
        assert_eq!(parse(r"\frac{a}"), Err(MathError::MissingArgument("frac")));
        assert_eq!(
            parse(r"\mathrm{x}"),
            Err(MathError::Unknown("mathrm".into()))
        );
        assert_eq!(parse(r"\text{a"), Err(MathError::Unbalanced));
    }
}
