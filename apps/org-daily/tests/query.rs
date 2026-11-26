use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn write(root: &Path, path: &str, content: impl AsRef<[u8]>) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn query(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_org-daily"))
        .arg("--dir")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn assert_status(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dates_are_inclusive_and_exclude_files_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "2024-02-29.org", "* 平铺\n");
    write(root, "2024/2024-03-01.org", "* 旧版\n");
    write(root, "2024/03/2024-03-02.org", "* 现在\n");
    write(root, "2024/03/2024-03-03.org", [0xff]);
    write(root, "2024/01/2024-01-02.org", [0xff]);
    write(root, "2023/2023-03-01.org", [0xff]);
    write(root, "2024/03/2024-02-30.org", [0xff]);
    write(root, "2024/03/.template.org", [0xff]);
    let output = query(root, &["--from", "2024-02-29", "--to", "2024-03-02"]);
    assert_status(&output, 0);
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].ends_with("2024-03-02\t现在"));
    assert!(lines[1].ends_with("2024-03-01\t旧版"));
    assert!(lines[2].ends_with("2024-02-29\t平铺"));
}

#[test]
fn metadata_and_body_match_one_heading_without_mixing_children() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        ".setup.org",
        "#+SETUPFILE: .setup.org\n#+FILETAGS: :日记:工作:\n#+TODO: WAIT(w) | DONE(d)\n",
    );
    write(
        root,
        "2026/11/2026-11-24.org",
        concat!(
            "#+SETUPFILE: ../../.setup.org\r\n",
            "#+FILETAGS: :测试:\r\n",
            "* 项目 :团队:\r\n",
            "** WAIT [#A] 查找 [alpha] :待办:工作:\r\n",
            "SCHEDULED: <2026-11-25 Wed>\r\n",
            ":PROPERTIES:\r\n",
            ":ID: fixture\r\n",
            ":AREA: product\r\n",
            ":area+: infrastructure\r\n",
            ":EMPTY:\r\n",
            ":END:\r\n",
            ":LOGBOOK:\r\n",
            "  隐藏\r\n",
            ":END:\r\n",
            "正文含关键字\r\n",
            "#+BEGIN_SRC text\r\n",
            "* WAIT 代码内容 :待办:\r\n",
            "#+END_SRC\r\n",
            "*** DONE 子任务 :待办:\r\n",
            "子任务另有内容\r\n",
            "* 普通标题\r\n",
        ),
    );

    let output = query(
        root,
        &[
            "--tasks",
            "--state",
            "WAIT",
            "--state",
            "DONE",
            "-Fi",
            "--title",
            "[ALPHA]",
            "--body",
            "关键字",
            "--tag",
            "团队",
            "--tag",
            "工作",
            "--tag",
            "待办",
            "--property",
            "id",
            "--property",
            "area=product infrastructure",
            "--property",
            "EMPTY=",
            "--format",
            "jsonl",
        ],
    );
    assert_status(&output, 0);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    let entry: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(entry["line"], 4);
    assert_eq!(entry["level"], 2);
    assert_eq!(entry["title"], "查找 [alpha]");
    assert_eq!(entry["state"], "WAIT");
    assert_eq!(entry["outline"], json!(["项目"]));
    assert_eq!(entry["properties"]["ID"], "fixture");
    assert_eq!(
        entry["tags"],
        json!(["团队", "工作", "待办", "日记", "测试"])
    );
    assert_eq!(entry["body"], "正文含关键字\n* WAIT 代码内容 :待办:");

    let output = query(root, &["--title", "^查找", "--body", "子任务另有内容"]);
    assert_status(&output, 1);
    assert!(output.stdout.is_empty());
    assert_status(&query(root, &["--title", "^子任务", "--property", "ID"]), 1);
}

#[test]
fn summary_counts_matching_states_and_deduplicated_tags() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "2026-11-24.org",
        "#+FILETAGS: :topic:\n* TODO 甲 :topic:\n正文\n* DONE 乙\n正文\n* 丙\n",
    );
    let output = query(
        root,
        &["--tasks", "--body", "^正文$", "--format", "summary"],
    );
    assert_status(&output, 0);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "记录数\t2\n日期\t2026-11-24\t2\n状态\tDONE\t1\n状态\tTODO\t1\n标签\ttopic\t2\n"
    );
    assert_status(&query(root, &["--from", "2026-02-29"]), 2);
    assert_status(
        &query(root, &["--from", "2026-11-25", "--to", "2026-11-24"]),
        2,
    );
    assert_status(&query(root, &["--title", "["]), 2);
}

#[test]
fn where_composes_with_title_regex_and_inherited_tags() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "2026-11-24.org",
        concat!(
            "#+FILETAGS: :日记:\n",
            "* 项目 :工作:\n",
            "** TODO 阅读 A1\n",
            ":PROPERTIES:\n",
            ":AREA: Research (API): v=1\n",
            ":NOTE:\n",
            ":END:\n",
            "** TODO 阅读 A2 :归档:\n",
            "* TODO 复盘 B1 :学习:\n",
            ":PROPERTIES:\n",
            ":AREA: Research (API): v=1\n",
            ":NOTE: completed \"today\"\n",
            ":END:\n",
            "* TODO 复盘 C1 :学习:\n",
        ),
    );

    for (expression, regex, expected) in [
        (
            "tag:工作 OR tag:学习 AND NOT tag:归档 AND prop:note=\"completed \\\"today\\\"\"",
            "^(阅读|复盘) [AB][12]$",
            vec!["阅读 A1", "阅读 A2", "复盘 B1"],
        ),
        (
            r#"(tag:工作 or tag:学习) AND NOT tag:归档 AND prop:area="Research (API): v=1""#,
            "^(阅读|复盘) [AB]1$",
            vec!["阅读 A1", "复盘 B1"],
        ),
        (r#"prop:note="""#, "^阅读", vec!["阅读 A1"]),
        ("NOT prop:note", "^复盘", vec!["复盘 C1"]),
    ] {
        let output = query(
            root,
            &[
                "--title", regex, "--where", expression, "--tag", "日记", "--format", "jsonl",
            ],
        );
        assert_status(&output, 0);
        let titles: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["title"].clone())
            .collect();
        assert_eq!(
            titles,
            expected
                .iter()
                .map(|title| json!(title))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn malformed_where_reports_a_position_before_reading_files() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "2026-11-24.org", [0xff]);
    for (expression, diagnostic) in [
        ("(tag:工作", "第 8 个字符处，缺少右括号"),
        ("tag:工作 tag:阅读", "第 8 个字符处"),
        ("tag:工作 AND", "此处应为 tag:标签"),
        ("prop:ID=", "缺少标签或属性值"),
        ("prop:ID+ = xyz", "属性名应为有效的 KEY"),
        (r#"prop:NOTE="未闭合"#, "双引号字符串无效"),
    ] {
        let output = query(dir.path(), &["--where", expression]);
        assert_status(&output, 2);
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(diagnostic), "{error}");
    }
}
