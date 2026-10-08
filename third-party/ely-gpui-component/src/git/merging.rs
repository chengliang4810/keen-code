use similar::{DiffableStr, MergeResolution, TextMerge};

/// Which side a conflict takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Ours,
    Theirs,
    Both,
}

/// How a merge region stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Unchanged,
    Ours,
    Theirs,
    /// Both sides made the same change.
    Both,
    Conflict,
}

/// A stretch of a three-way merge, with each side's lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub kind: RegionKind,
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

/// A region's lines as similar split them, each with its ending.
fn lines<'a>(
    range: std::ops::Range<usize>,
    line: impl Fn(usize) -> Option<&'a str>,
) -> Vec<String> {
    range
        .map(|ix| {
            line(ix)
                .expect("a region's lines are in its text")
                .to_string()
        })
        .collect()
}

/// The line ending the merged texts use: the first one found, or a newline.
fn ending(regions: &[Region]) -> &'static str {
    let line = regions
        .iter()
        .flat_map(|region| region.base.iter().chain(&region.ours).chain(&region.theirs))
        .find(|line| line.as_str().ends_with_newline());
    match line {
        Some(line) if line.ends_with("\r\n") => "\r\n",
        Some(line) if line.ends_with('\r') => "\r",
        _ => "\n",
    }
}

/// Ends the last line when it has no ending, so what follows starts a line.
fn break_line(out: &mut String, ending: &str) {
    if !out.is_empty() && !out.as_str().ends_with_newline() {
        out.push_str(ending);
    }
}

/// `ours` and `theirs` merged against `base`, region by region.
pub fn regions(base: &str, ours: &str, theirs: &str) -> Vec<Region> {
    let merge = TextMerge::from_lines(base, ours, theirs);
    merge
        .regions()
        .iter()
        .map(|region| Region {
            kind: match region.resolution() {
                MergeResolution::Unchanged => RegionKind::Unchanged,
                MergeResolution::Ours => RegionKind::Ours,
                MergeResolution::Theirs => RegionKind::Theirs,
                MergeResolution::Both => RegionKind::Both,
                MergeResolution::Conflict => RegionKind::Conflict,
                other => unreachable!("a merge resolution this crate does not know: {other:?}"),
            },
            base: lines(region.base_range(), |ix| merge.base_line(ix)),
            ours: lines(region.ours_range(), |ix| merge.ours_line(ix)),
            theirs: lines(region.theirs_range(), |ix| merge.theirs_line(ix)),
        })
        .collect()
}

/// The merged text with each conflict as taken, and how many are still open; an open one keeps its markers.
pub fn result(regions: &[Region], takes: &[Option<Take>]) -> (String, usize) {
    let ending = ending(regions);
    let mut out = String::new();
    let mut conflicts = 0;
    let mut open = 0;
    for region in regions {
        let (ours, theirs) = (region.ours.concat(), region.theirs.concat());
        match region.kind {
            RegionKind::Unchanged | RegionKind::Ours | RegionKind::Both => out.push_str(&ours),
            RegionKind::Theirs => out.push_str(&theirs),
            RegionKind::Conflict => {
                match takes.get(conflicts).copied().flatten() {
                    Some(Take::Ours) => out.push_str(&ours),
                    Some(Take::Theirs) => out.push_str(&theirs),
                    Some(Take::Both) => {
                        out.push_str(&ours);
                        if !theirs.is_empty() {
                            break_line(&mut out, ending);
                        }
                        out.push_str(&theirs);
                    }
                    None => {
                        open += 1;
                        for (marker, side) in [("<<<<<<< ours", &ours), ("=======", &theirs)] {
                            break_line(&mut out, ending);
                            out.push_str(marker);
                            out.push_str(ending);
                            out.push_str(side);
                        }
                        break_line(&mut out, ending);
                        out.push_str(">>>>>>> theirs");
                        out.push_str(ending);
                    }
                }
                conflicts += 1;
            }
        }
    }
    (out, open)
}

/// A conflict in marked text: the lines of its markers, from zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub start: usize,
    pub middle: usize,
    pub end: usize,
}

/// The conflicts marked in `text`: `<<<<<<<`, `=======`, `>>>>>>>`, each at a line's start.
pub fn conflicts(text: &str) -> Vec<Conflict> {
    let mut found = Vec::new();
    let (mut start, mut middle) = (None, None);
    for (ix, line) in text.tokenize_lines().into_iter().enumerate() {
        if line.starts_with("<<<<<<<") {
            (start, middle) = (Some(ix), None);
        } else if line.starts_with("=======") && start.is_some() {
            middle = Some(ix);
        } else if line.starts_with(">>>>>>>")
            && let (Some(start), Some(middle)) = (start.take(), middle.take())
        {
            found.push(Conflict {
                start,
                middle,
                end: ix,
            });
        }
    }
    found
}

/// `text` with conflict `ix` settled as taken; every other byte stays.
pub fn resolve(text: &str, ix: usize, take: Take) -> String {
    let conflict = conflicts(text)[ix];
    let lines = text.tokenize_lines();
    let ours = lines[conflict.start + 1..conflict.middle].concat();
    let theirs = lines[conflict.middle + 1..conflict.end].concat();
    let kept = match take {
        Take::Ours => ours,
        Take::Theirs => theirs,
        Take::Both => ours + &theirs,
    };
    log::info!("conflict {ix} took {take:?}");
    [
        lines[..conflict.start].concat(),
        kept,
        lines[conflict.end + 1..].concat(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "fn lift() {\n    0.5\n}\nfn keep() {}\n";
    const OURS: &str = "fn lift() {\n    0.6\n}\nfn keep() {}\n";
    const THEIRS: &str = "fn lift() {\n    0.75\n}\nfn keep() { log() }\n";

    #[test]
    fn a_merge_keeps_clean_changes_and_holds_conflicts() {
        let all = regions(BASE, OURS, THEIRS);
        let kinds: Vec<RegionKind> = all.iter().map(|region| region.kind).collect();
        assert!(kinds.contains(&RegionKind::Conflict) && kinds.contains(&RegionKind::Theirs));
        let (open, count) = result(&all, &[]);
        assert_eq!(count, 1);
        assert!(open.contains("<<<<<<< ours\n    0.6\n=======\n    0.75\n>>>>>>> theirs"));
        let (taken, count) = result(&all, &[Some(Take::Theirs)]);
        assert_eq!(count, 0);
        assert_eq!(taken, "fn lift() {\n    0.75\n}\nfn keep() { log() }\n");
    }

    #[test]
    fn merges_keep_line_endings_and_the_last_newline() {
        assert_eq!(result(&regions("x", "x", "x"), &[]), ("x".into(), 0));
        let crlf = regions("a\r\nb\r\n", "a\r\nB\r\n", "a\r\nb\r\n");
        assert_eq!(result(&crlf, &[]).0, "a\r\nB\r\n");
        let held = regions("a\r\n", "b\r\n", "c\r\n");
        assert_eq!(
            result(&held, &[]).0,
            "<<<<<<< ours\r\nb\r\n=======\r\nc\r\n>>>>>>> theirs\r\n"
        );
        let bare = regions("a", "b", "c");
        assert_eq!(result(&bare, &[Some(Take::Both)]).0, "b\nc");
        let marked = "a\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> topic\r\nz";
        assert_eq!(resolve(marked, 0, Take::Ours), "a\r\nours\r\nz");
    }

    #[test]
    fn a_bare_return_ends_a_line_as_it_does_for_similar() {
        let changed = regions("a\rb\n", "a\rb\n", "a\rB\n");
        assert_eq!(result(&changed, &[]).0, "a\rB\n");
        let held = result(&regions("a\r", "b\r", "c\r"), &[]).0;
        assert_eq!(held, "<<<<<<< ours\rb\r=======\rc\r>>>>>>> theirs\r");
        assert_eq!(resolve(&held, 0, Take::Theirs), "c\r");
    }

    #[test]
    fn taking_both_adds_no_line_to_an_empty_side() {
        assert_eq!(result(&regions("a", "b", ""), &[Some(Take::Both)]).0, "b");
        assert_eq!(result(&regions("a", "", "c"), &[Some(Take::Both)]).0, "c");
    }

    #[test]
    fn markers_resolve_to_one_side_or_both() {
        let marked = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> topic\nz\n";
        assert_eq!(
            conflicts(marked),
            [Conflict {
                start: 1,
                middle: 3,
                end: 5
            }]
        );
        assert_eq!(resolve(marked, 0, Take::Ours), "a\nours\nz\n");
        assert_eq!(resolve(marked, 0, Take::Both), "a\nours\ntheirs\nz\n");
        assert!(
            conflicts("a\n=======\nb\n").is_empty(),
            "a rule alone is no conflict"
        );
    }
}
