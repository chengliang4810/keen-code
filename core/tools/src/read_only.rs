//! Shell 命令的只读判定：决定一次命令调用能否与其他只读调用并行。
//!
//! 判定采用 fail-closed 策略：只有能明确证明"不写状态"的命令才判为只读，
//! 任何解析失败、未知命令、未知选项、重定向写文件、命令替换、后台执行都
//! 判为可写。判错的方向性代价并不对称——把只读误判为可写只损失一点并行度，
//! 把可写误判为只读则会破坏顺序副作用屏障（例如 `rm` 与 `cat` 同批并发）。
//!
//! 本模块只服务并发调度，不参与权限或审批：KeenCode 的工具执行不设审批分支，
//! 因此这里不需要也不可能做到"安全沙箱"级别的精确度。

/// 只读判定允许的最大命令长度。
///
/// 超长命令通常由脚本拼接产生，静态判定的可信度下降，直接按可写处理。
pub(crate) const MAX_CLASSIFIABLE_COMMAND_BYTES: usize = 8 * 1024;

/// 一次只读判定的结论。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadOnlyVerdict {
    /// 命令不改变外部状态，可以与其他只读调用并发。
    ReadOnly,
    /// 命令可能改变状态，或无法证明其只读，必须独占执行。
    Writes,
}

/// 判断一条 Bash 命令是否只读。
///
/// 输入是模型的原始命令字符串；判定不执行任何命令，只做静态分析。
pub(crate) fn classify_bash_command(command: &str) -> ReadOnlyVerdict {
    // 空命令、含换行的多行脚本、含命令替换或后台符号的输入一律判为可写：
    // 这些结构无法在单条命令的粒度上静态确认。
    if command.trim().is_empty() {
        return ReadOnlyVerdict::Writes;
    }
    if command.len() > MAX_CLASSIFIABLE_COMMAND_BYTES {
        return ReadOnlyVerdict::Writes;
    }
    let segments = match split_command_segments(command) {
        Some(segments) => segments,
        None => return ReadOnlyVerdict::Writes,
    };
    if segments.is_empty() {
        return ReadOnlyVerdict::Writes;
    }
    // 管道与顺序执行本身不写状态，但其两侧的每条命令都必须各自只读。
    for segment in segments {
        match classify_segment(&segment) {
            ReadOnlyVerdict::ReadOnly => {}
            ReadOnlyVerdict::Writes => return ReadOnlyVerdict::Writes,
        }
    }
    ReadOnlyVerdict::ReadOnly
}

/// 把命令按管道与顺序操作符切成若干段。
///
/// 返回 `None` 表示命令含有无法静态切分的结构（命令替换、子 shell、后台
/// 执行、写重定向等），调用方应直接判为可写。引号内的操作符不作为分隔符。
fn split_command_segments(command: &str) -> Option<Vec<Vec<String>>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let mut pending_escaped = false;

    while let Some(character) = chars.next() {
        if pending_escaped {
            // 反斜杠转义的字符按字面量进入当前词，不作为操作符。
            word.push(character);
            pending_escaped = false;
            continue;
        }
        match quote {
            Some(active) => {
                if character == '\\' && active == '"' {
                    pending_escaped = true;
                    continue;
                }
                if character == active {
                    quote = None;
                    continue;
                }
                word.push(character);
            }
            None => match character {
                '\'' | '"' => quote = Some(character),
                '\\' => pending_escaped = true,
                '#' if word.is_empty() => {
                    // 行首注释：其后的内容不参与命令。
                    for rest in chars.by_ref() {
                        if rest == '\n' {
                            break;
                        }
                    }
                }
                '\n' | ';' => {
                    flush_word(&mut word, &mut current);
                    push_segment(&mut segments, &mut current);
                }
                '&' => {
                    // `&&` 与 `&` 都结束当前段；`&&` 只是顺序执行的一种。
                    flush_word(&mut word, &mut current);
                    push_segment(&mut segments, &mut current);
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    } else {
                        // 单独的后台执行 `&`：进程生命周期不可控。
                        return None;
                    }
                }
                '|' => {
                    flush_word(&mut word, &mut current);
                    push_segment(&mut segments, &mut current);
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                }
                '>' => {
                    // 写重定向可能创建或覆盖文件，整条命令判为可写。
                    return None;
                }
                '<' => {
                    // 输入重定向只读，但 heredoc 体与后续内容无法在词级确认。
                    return None;
                }
                '(' | ')' | '{' | '}' => return None,
                '$' => {
                    if chars.peek() == Some(&'(') {
                        // 命令替换的结果不可静态确认。
                        return None;
                    }
                    word.push(character);
                }
                '`' => return None,
                _ if character.is_whitespace() => flush_word(&mut word, &mut current),
                _ => word.push(character),
            },
        }
    }
    if quote.is_some() {
        // 引号未闭合：解析失败，按可写处理。
        return None;
    }
    if pending_escaped {
        word.push('\\');
    }
    flush_word(&mut word, &mut current);
    push_segment(&mut segments, &mut current);
    Some(segments)
}

/// 把当前词收进当前段，并重置词缓冲。
fn flush_word(word: &mut String, current: &mut Vec<String>) {
    if !word.is_empty() {
        current.push(std::mem::take(word));
    }
}

/// 把当前段收进结果集，并跳过空段。
fn push_segment(segments: &mut Vec<Vec<String>>, current: &mut Vec<String>) {
    if !current.is_empty() {
        segments.push(std::mem::take(current));
    }
}

/// 判断单个命令段（无管道、无控制操作符）是否只读。
fn classify_segment(argv: &[String]) -> ReadOnlyVerdict {
    let argv = strip_assignments(argv);
    let Some(program) = argv.first() else {
        return ReadOnlyVerdict::Writes;
    };
    // 带路径的命令按基名匹配（/usr/bin/git 与 git 同义）。
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program.as_str());
    let args = &argv[1..];
    match name {
        // 只读文本查看：任何参数都不写状态，但重定向已在切分阶段拦截。
        "cat" | "bat" | "head" | "tail" | "less" | "more" | "wc" | "nl" | "tac" | "column"
        | "cut" | "paste" | "join" | "comm" | "tr" | "rev" | "seq" | "jq" | "yq" => {
            ReadOnlyVerdict::ReadOnly
        }
        // 文件系统只读查询。
        "ls" | "dir" | "pwd" | "stat" | "file" | "du" | "df" | "tree" | "realpath" | "basename"
        | "dirname" | "readlink" => ReadOnlyVerdict::ReadOnly,
        // 进程与系统只读查询。
        "ps" | "top" | "free" | "uptime" | "whoami" | "id" | "groups" | "hostname" | "uname"
        | "date" | "env" | "printenv" | "which" | "where" | "type" | "command" | "echo"
        | "printf" | "true" | "false" | "sleep" => ReadOnlyVerdict::ReadOnly,
        // 文本搜索与比较：grep 的 -r 只读；diff/cmp 只读。
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "diff" | "cmp" | "md5sum"
        | "sha1sum" | "sha256sum" | "shasum" | "cksum" | "base64" | "xxd" | "od" | "hexdump"
        | "strings" | "sort" | "uniq" | "sed" | "awk" | "gawk" | "mawk" => {
            classify_filter(name, args)
        }
        // 目录树查找：find 只在不含写动作时只读。
        "find" => classify_find(args),
        // 版本控制：只有明确只读的子命令才并行。
        "git" => classify_git(args),
        // 其余命令（含 rm/mv/cp/mkdir/touch/chmod/npm/cargo/make/docker 等）
        // 一律判为可写。
        _ => ReadOnlyVerdict::Writes,
    }
}

/// 去掉命令前置的 `VAR=value` 环境赋值。
///
/// 赋值只影响本次进程环境，不改变外部状态，因此不影响只读判定；但如果
/// 赋值后没有命令（纯赋值），整段不产生任何动作，仍按可写处理以避免空命令段
/// 被误判成"只读执行"。
fn strip_assignments(argv: &[String]) -> &[String] {
    let mut index = 0;
    while index < argv.len() {
        let token = &argv[index];
        let Some((name, _)) = token.split_once('=') else {
            break;
        };
        // 合法的环境变量名：字母或下划线开头，其余为字母数字下划线。
        let mut characters = name.chars();
        let valid = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|rest| rest.is_ascii_alphanumeric() || rest == '_');
        if !valid {
            break;
        }
        index += 1;
    }
    &argv[index..]
}

/// 判定 sed/awk 一类"默认只读但选项可写"的过滤器。
///
/// 关键区分是原地写：`sed -i`、`awk` 的 `-i inplace` 会改文件，必须排除。
fn classify_filter(name: &str, args: &[String]) -> ReadOnlyVerdict {
    for arg in args {
        // `-i` 及 `-i<suffix>` 都是原地编辑。
        if arg == "-i" || arg.starts_with("-i") && !arg.starts_with("--") {
            return ReadOnlyVerdict::Writes;
        }
        if arg == "--in-place" || arg.starts_with("--in-place=") {
            return ReadOnlyVerdict::Writes;
        }
        // sed 的 `w`/`W` 命令与 `e` 命令会写文件或执行命令。
        if name == "sed" && (arg.contains("w ") || arg.contains("e ")) {
            return ReadOnlyVerdict::Writes;
        }
        if name.starts_with("awk") || name == "gawk" || name == "mawk" {
            // awk 的 print > "file" 与 system() 会写状态。
            if arg.contains('>') || arg.contains("system(") {
                return ReadOnlyVerdict::Writes;
            }
        }
    }
    ReadOnlyVerdict::ReadOnly
}

/// 判定 `find` 是否只读：任何执行或删除动作都判为可写。
fn classify_find(args: &[String]) -> ReadOnlyVerdict {
    const WRITE_ACTIONS: &[&str] = &[
        "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fls", "-fprint", "-fprint0", "-fprintf",
    ];
    for arg in args {
        if WRITE_ACTIONS.contains(&arg.as_str()) {
            return ReadOnlyVerdict::Writes;
        }
    }
    ReadOnlyVerdict::ReadOnly
}

/// 判定 git 子命令是否只读。
///
/// 白名单只收录不改变仓库或工作区状态的子命令；`git` 的多数子命令都可能
/// 写状态（含 `branch`、`tag`、`stash`、`config` 等看似无害的命令），因此
/// 未列出的子命令一律判为可写。
fn classify_git(args: &[String]) -> ReadOnlyVerdict {
    let mut index = 0;
    // 跳过全局选项及其参数，定位真正的子命令。
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                index += 2;
            }
            "--no-pager"
            | "-P"
            | "--paginate"
            | "--no-replace-objects"
            | "--bare"
            | "--literal-pathspecs"
            | "--no-optional-locks" => {
                index += 1;
            }
            _ if arg.starts_with('-') => {
                // 未知全局选项无法确认其语义。
                return ReadOnlyVerdict::Writes;
            }
            _ => break,
        }
    }
    let Some(subcommand) = args.get(index) else {
        return ReadOnlyVerdict::Writes;
    };
    let rest = &args[index + 1..];
    match subcommand.as_str() {
        "status" | "log" | "show" | "diff" | "grep" | "blame" | "shortlog" | "describe"
        | "rev-parse" | "rev-list" | "ls-files" | "ls-tree" | "cat-file" | "show-ref"
        | "for-each-ref" | "name-rev" | "whatchanged" | "reflog" | "cherry" | "merge-base"
        | "count-objects" | "verify-commit" | "verify-tag" | "symbolic-ref" | "var" => {
            // `git diff` 等支持 --output=<file>，会写文件。
            for arg in rest {
                if arg.starts_with("--output") {
                    return ReadOnlyVerdict::Writes;
                }
            }
            ReadOnlyVerdict::ReadOnly
        }
        // 这些子命令既能查询也能修改：只有参数里不含写动作时才只读。
        "branch" => classify_git_branch(rest),
        "tag" => classify_git_tag(rest),
        "remote" => classify_git_remote(rest),
        "stash" => classify_git_stash(rest),
        "worktree" => classify_git_worktree(rest),
        "submodule" => classify_git_submodule(rest),
        _ => ReadOnlyVerdict::Writes,
    }
}

/// `git branch` 只有列举形式只读；任何创建、删除、改名或设置上游都会写仓库。
fn classify_git_branch(args: &[String]) -> ReadOnlyVerdict {
    const WRITE_FLAGS: &[&str] = &[
        "-d",
        "-D",
        "--delete",
        "-m",
        "-M",
        "--move",
        "-c",
        "-C",
        "--copy",
        "--set-upstream-to",
        "-u",
        "--unset-upstream",
        "--edit-description",
    ];
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if WRITE_FLAGS.contains(&arg.as_str()) {
            return ReadOnlyVerdict::Writes;
        }
        if arg == "-a"
            || arg == "--all"
            || arg == "-r"
            || arg == "--remotes"
            || arg == "-v"
            || arg == "-vv"
            || arg == "--verbose"
            || arg == "--list"
            || arg == "--show-current"
            || arg == "--contains"
            || arg == "--merged"
            || arg == "--no-merged"
            || arg == "--points-at"
            || arg == "--format"
        {
            index += 1;
            continue;
        }
        if arg.starts_with("--format=") || arg.starts_with("--contains=") {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            // 未知选项按可写处理。
            return ReadOnlyVerdict::Writes;
        }
        // 出现位置参数说明是"创建分支"形态。
        return ReadOnlyVerdict::Writes;
    }
    ReadOnlyVerdict::ReadOnly
}

/// `git tag` 只有列举形式只读。
fn classify_git_tag(args: &[String]) -> ReadOnlyVerdict {
    for arg in args {
        match arg.as_str() {
            "-l" | "--list" | "-n" | "--contains" | "--merged" | "--no-merged" | "--points-at"
            | "--sort" | "--format" | "-d" | "--delete" => {
                if arg == "-d" || arg == "--delete" {
                    return ReadOnlyVerdict::Writes;
                }
            }
            _ if arg.starts_with("--list=")
                || arg.starts_with("--sort=")
                || arg.starts_with("--format=")
                || arg.starts_with("-n") =>
            {
                continue;
            }
            _ if arg.starts_with('-') => return ReadOnlyVerdict::Writes,
            // 位置参数 = 创建标签。
            _ => return ReadOnlyVerdict::Writes,
        }
    }
    ReadOnlyVerdict::ReadOnly
}

/// `git remote` 只有 `-v`/`show`/`get-url` 只读。
fn classify_git_remote(args: &[String]) -> ReadOnlyVerdict {
    match args.first().map(String::as_str) {
        Some("-v" | "--verbose" | "show" | "get-url") => ReadOnlyVerdict::ReadOnly,
        // 无参数 = 列举远端名，只读。
        None => ReadOnlyVerdict::ReadOnly,
        _ => ReadOnlyVerdict::Writes,
    }
}

/// `git stash` 只有 `list`/`show` 只读。
fn classify_git_stash(args: &[String]) -> ReadOnlyVerdict {
    match args.first().map(String::as_str) {
        Some("list" | "show") => ReadOnlyVerdict::ReadOnly,
        _ => ReadOnlyVerdict::Writes,
    }
}

/// `git worktree` 只有 `list` 只读。
fn classify_git_worktree(args: &[String]) -> ReadOnlyVerdict {
    match args.first().map(String::as_str) {
        Some("list") => ReadOnlyVerdict::ReadOnly,
        _ => ReadOnlyVerdict::Writes,
    }
}

/// `git submodule` 只有 `status`/`summary` 只读。
fn classify_git_submodule(args: &[String]) -> ReadOnlyVerdict {
    match args.first().map(String::as_str) {
        Some("status" | "summary") => ReadOnlyVerdict::ReadOnly,
        _ => ReadOnlyVerdict::Writes,
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_CLASSIFIABLE_COMMAND_BYTES, ReadOnlyVerdict, classify_bash_command};

    /// 断言命令被判为只读。
    fn assert_read_only(command: &str) {
        assert_eq!(
            classify_bash_command(command),
            ReadOnlyVerdict::ReadOnly,
            "应判为只读：{command}"
        );
    }

    /// 断言命令被判为可写。
    fn assert_writes(command: &str) {
        assert_eq!(
            classify_bash_command(command),
            ReadOnlyVerdict::Writes,
            "应判为可写：{command}"
        );
    }

    #[test]
    fn common_inspection_commands_are_read_only() {
        for command in [
            "ls -la",
            "cat README.md",
            "head -n 20 src/main.rs",
            "tail -f log.txt",
            "wc -l src/lib.rs",
            "pwd",
            "git status",
            "git log --oneline -20",
            "git diff HEAD~1",
            "git show HEAD",
            "grep -rn TODO src",
            "rg --files",
            "find . -name '*.rs'",
            "sed -n '1,10p' file.txt",
            "awk '{print $1}' file.txt",
        ] {
            assert_read_only(command);
        }
    }

    #[test]
    fn pipes_and_sequences_require_every_segment_read_only() {
        assert_read_only("cat file.txt | wc -l");
        assert_read_only("ls -la && git status");
        assert_read_only("git log --oneline | head -n 5");
        // 只要有一段可写，整条命令即判为可写。
        assert_writes("cat file.txt | tee copy.txt");
        assert_writes("ls -la && rm -rf build");
        assert_writes("git status; git commit -m x");
    }

    #[test]
    fn write_redirects_and_substitutions_are_never_read_only() {
        assert_writes("ls > files.txt");
        assert_writes("cat file.txt >> log.txt");
        assert_writes("echo hi > out.txt");
        assert_writes("cat $(ls)");
        assert_writes("echo `date`");
        assert_writes("cat < input.txt");
        assert_writes("ls &");
        assert_writes("(ls)");
    }

    #[test]
    fn mutating_commands_are_writes() {
        for command in [
            "rm -rf build",
            "mv a b",
            "cp a b",
            "mkdir -p out",
            "touch file.txt",
            "chmod +x run.sh",
            "npm install",
            "cargo build",
            "make",
            "docker ps",
            "git commit -m 'x'",
            "git push",
            "git checkout main",
            "git reset --hard",
            "git clean -fd",
            "git add .",
            "git stash",
            "git branch new-branch",
            "git tag v1.0",
            "git remote add origin url",
            "find . -name '*.tmp' -delete",
            "find . -exec rm {} ;",
            "sed -i 's/a/b/' file.txt",
            "sed -i.bak 's/a/b/' file.txt",
        ] {
            assert_writes(command);
        }
    }

    #[test]
    fn git_read_only_subcommands_are_recognized_with_global_flags() {
        assert_read_only("git -C /repo status");
        assert_read_only("git --no-pager log");
        assert_read_only("git branch");
        assert_read_only("git branch -a");
        assert_read_only("git branch --list");
        assert_read_only("git tag -l");
        assert_read_only("git remote -v");
        assert_read_only("git stash list");
        assert_read_only("git worktree list");
        assert_read_only("git submodule status");
        // 写形态仍必须判为可写。
        assert_writes("git branch -d old");
        assert_writes("git tag -d v1.0");
        assert_writes("git stash pop");
        assert_writes("git diff --output=patch.diff");
        // 未知全局选项无法确认语义。
        assert_writes("git --unknown-flag status");
    }

    #[test]
    fn environment_assignments_do_not_hide_the_program() {
        assert_read_only("FOO=1 ls -la");
        assert_read_only("LC_ALL=C grep pattern file.txt");
        // 纯赋值没有命令，不产生可判定的只读动作。
        assert_writes("FOO=1");
    }

    #[test]
    fn quoted_operators_are_not_treated_as_separators() {
        assert_read_only("grep 'a && b' file.txt");
        assert_read_only("echo 'hello; world'");
        assert_read_only("git log --grep='fix: a > b'");
    }

    #[test]
    fn malformed_input_fails_closed() {
        assert_writes("");
        assert_writes("   ");
        assert_writes("cat 'unterminated");
        assert_writes("unknown-command --flag");
        // 超长命令按可写处理，避免在拼接脚本上做低置信度判定。
        let long = format!("ls {}", "a".repeat(MAX_CLASSIFIABLE_COMMAND_BYTES));
        assert_writes(&long);
    }
}
