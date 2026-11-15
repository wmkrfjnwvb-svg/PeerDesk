//! 把管理页面的构建产物嵌进服务端二进制。
//!
//! 有 `web/dist`（`npm run build` 的输出）就嵌它；没有就嵌一个占位页面，
//! 这样不装 Node 也能编译服务端。
#![allow(clippy::print_stdout)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let dist = manifest.join("../../web/dist");
    let placeholder = manifest.join("placeholder");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed={}", placeholder.display());

    let root = if dist.join("index.html").is_file() {
        dist.canonicalize()?
    } else {
        placeholder.canonicalize()?
    };
    let mut files = Vec::new();
    collect(&root, &mut files)?;
    files.sort();

    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for file in &files {
        let rel = file
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        writeln!(
            code,
            "    ({:?}, include_bytes!({:?})),",
            format!("/{rel}"),
            file
        )?;
    }
    code.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR")?).join("assets.rs"), code)?;
    Ok(())
}
