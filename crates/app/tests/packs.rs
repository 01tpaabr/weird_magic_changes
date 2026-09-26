//! Rule packs and saves, on the real binary: a save remembers the packs it
//! was played with, opens under packs that number its kinds differently
//! (matched by name), falls back to the built-in rules when its packs are
//! gone, and is refused by rules that lack its kinds. Opening never writes.

use std::path::Path;
use std::process::Command;

use sim_core::{Scenario, Store, sim};

/// `wmc args` with no `WMC_RULES`: success, stdout, stderr.
fn wmc(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .env("WMC_THREADS", "4")
        .args(args)
        .output()
        .expect("wmc runs");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

fn line(output: &str, key: &str) -> String {
    output
        .lines()
        .find(|l| l.starts_with(key))
        .unwrap_or_else(|| panic!("no {key} line in:\n{output}"))
        .to_string()
}

#[test]
fn a_save_remembers_its_packs_and_opens_by_name() {
    let root = std::env::temp_dir().join(format!("wmc-packs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // The repository's rules, copied into a pack of their own.
    let base = root.join("base");
    std::fs::create_dir_all(&base).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    for e in std::fs::read_dir(&repo).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, base.join(p.file_name().unwrap())).unwrap();
    }
    let kinds = sim_core::rules::compile_packs(&[&base]).unwrap();
    let store = Store::open(root.join("save")).unwrap();
    let scenario = Scenario {
        width: 128,
        height: 128,
        seed: 12,
        ..Scenario::builtin()
    };
    let mut w = sim::new_world_with(&scenario, kinds.clone()).unwrap();
    for _ in 0..100 {
        sim::step(&mut w);
    }
    sim::save(&mut w, &store).unwrap();
    let saved = store.read_meta().unwrap().unwrap();
    assert_eq!(saved.packs, kinds.debug.packs);
    let dir = store.dir().to_str().unwrap();

    // No --rules: the save's own packs.
    let (ok, plain, err) = wmc(&["run", dir, "400"]);
    assert!(ok, "{err}");
    assert!(err.contains("rules: the save's packs"), "{err}");

    // A pack after it whose kind extends `fox`: numbered right after it, so
    // every later kind id moves. The same world by name, the same run.
    let wolves = root.join("wolves");
    std::fs::create_dir_all(&wolves).unwrap();
    std::fs::write(
        wolves.join("w.rules"),
        "kind aardwolf extends fox { glyph \"W\" }",
    )
    .unwrap();
    let (b, w) = (base.to_str().unwrap(), wolves.to_str().unwrap());
    let (ok, wolf, err) = wmc(&["run", dir, "400", "--rules", b, "--rules", w]);
    assert!(ok, "{err}");
    assert!(err.contains("rules: changed since this save"), "{err}");
    assert_eq!(
        line(&wolf, "chunks:").replace(" aardwolf 0,", ""),
        line(&plain, "chunks:")
    );
    assert_ne!(line(&wolf, "state:"), line(&plain, "state:"), "ids moved");

    // Rules without most of its kinds: refused, with the list.
    let plants = root.join("plants");
    std::fs::create_dir_all(&plants).unwrap();
    for f in ["plants.rules", "lib.rules"] {
        std::fs::copy(base.join(f), plants.join(f)).unwrap();
    }
    let (ok, _, err) = wmc(&["run", dir, "1", "--rules", plants.to_str().unwrap()]);
    assert!(!ok);
    assert!(
        err.contains(
            "the save has kinds the loaded rules do not define: chicken, chick, egg, fox, flower, hive, bee, grass"
        ),
        "{err}"
    );

    // Its packs gone: the built-in rules, said so.
    std::fs::remove_dir_all(&base).unwrap();
    let (ok, builtin, err) = wmc(&["run", dir, "400"]);
    assert!(ok, "{err}");
    assert!(err.contains("the save's packs are missing"), "{err}");
    assert_eq!(line(&builtin, "state:"), line(&plain, "state:"));

    // None of that wrote anything.
    assert_eq!(store.read_meta().unwrap().unwrap(), saved);
    std::fs::remove_dir_all(&root).unwrap();
}
