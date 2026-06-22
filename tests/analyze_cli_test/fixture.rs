use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::support::{run_cli, stderr_str, write};

/// A small TypeScript project with known analysis ground truth:
/// - the call chain `main → compute → helper`,
/// - the mutual-recursion pair `ping ↔ pong`,
/// - `compute` as the most complex function (loop + branch).
pub(crate) fn write_fixture(root: &Path) {
    write(
        &root.join("src/util.ts"),
        r#"export function helper(x: number): number {
  if (x > 3) {
    return x * 2;
  }
  return x + 1;
}

export function compute(x: number): number {
  let total = 0;
  for (let i = 0; i < x; i++) {
    if (i % 2 === 0) {
      total += helper(i);
    } else {
      total -= 1;
    }
  }
  return total;
}

export function ping(n: number): number {
  return n <= 0 ? 0 : pong(n - 1);
}

export function pong(n: number): number {
  return n <= 0 ? 1 : ping(n - 1);
}
"#,
    );
    write(
        &root.join("src/main.ts"),
        r#"import { compute } from './util';

export function main(): number {
  return compute(10);
}
"#,
    );
}

/// `codegraph init` builds the index by default; assert it worked.
pub(crate) fn init_fixture(root: &Path) {
    write_fixture(root);
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

/// A fixture with known trait/type/generic/taint ground truth:
/// - `Shape` interface implemented by `Circle` and `Square`,
/// - `totalArea(shapes: Shape[])` (UsesType → trait expansion),
/// - `identity<T>` (signature-heuristic generic),
/// - `readUserInput` → `execQuery` via `pipeline` (taint naming).
pub(crate) fn write_close_fixture(root: &Path) {
    write(
        &root.join("src/shapes.ts"),
        r#"export interface Shape {
  area(): number;
}

export class Circle implements Shape {
  radius: number = 1;
  area(): number {
    return 3.14 * this.radius * this.radius;
  }
}

export class Square implements Shape {
  side: number = 2;
  area(): number {
    return this.side * this.side;
  }
}

export function totalArea(shapes: Shape[]): number {
  let total = 0;
  for (const shape of shapes) {
    total += shape.area();
  }
  return total;
}

export function identity<T>(value: T): T {
  return value;
}
"#,
    );
    write(
        &root.join("src/io.ts"),
        r#"export function readUserInput(): string {
  return "input";
}

export function execQuery(sql: string): void {
}

export function pipeline(): void {
  execQuery(readUserInput());
}
"#,
    );
}

pub(crate) fn init_close_fixture(root: &Path) {
    write_close_fixture(root);
    let out = run_cli(root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
}

/// Run `git` in the fixture with identity pinned (CI has no global config).
pub(crate) fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "user.email=test@codegraph.test",
            "-c",
            "user.name=codegraph-test",
        ])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// LCOV covering helper/compute (and main) but not ping/pong.
pub(crate) fn write_lcov(root: &Path) -> String {
    let lcov = root.join("lcov.info");
    fs::write(
        &lcov,
        "SF:src/util.ts\nDA:1,5\nDA:2,5\nDA:3,2\nDA:5,1\nDA:8,3\nDA:9,4\nDA:10,4\nDA:16,1\nend_of_record\n\
         SF:src/main.ts\nDA:3,1\nDA:4,1\nend_of_record\n",
    )
    .unwrap();
    lcov.display().to_string()
}
