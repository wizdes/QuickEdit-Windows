//! Saves a picture of every control and state, through the real renderer.
//!
//! `cargo run -p qe-ui --locked --example gallery -- --out target/gallery`
#[path = "../tests/support/mod.rs"]
mod support;

use std::{env, fs, path::PathBuf};
use support::{png, render, scenes};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let out = args
        .windows(2)
        .find(|a| a[0] == "--out")
        .map(|a| PathBuf::from(&a[1]))
        .unwrap_or_else(|| PathBuf::from("target/gallery"));
    fs::create_dir_all(&out)?;
    for (theme_name, theme) in scenes::THEMES {
        for scene in scenes::all(&theme()) {
            let image = render(&scene.list, scene.width, scene.height);
            let path = out.join(format!("{}-{theme_name}.png", scene.name));
            fs::write(&path, png::encode(&image))?;
            println!("{}", path.display());
        }
    }
    Ok(())
}
