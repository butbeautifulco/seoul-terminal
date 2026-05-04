use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn main() {
    let proto_files = &[
        "src/messages.rs",
        "src/frame.rs",
        "src/session.rs",
        "src/resources.rs",
    ];

    let mut hasher = DefaultHasher::new();
    for path in proto_files {
        println!("cargo:rerun-if-changed={path}");
        if let Ok(content) = std::fs::read_to_string(path) {
            content.hash(&mut hasher);
        }
    }

    let hash = hasher.finish();
    println!("cargo:rustc-env=SEOUL_PROTO_HASH={hash:016x}");
}
