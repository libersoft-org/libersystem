#!/usr/bin/env python3
"""Exercise the production root matcher and role selector without starting a guest."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def item(source: str, start: str) -> str:
    begin = source.index(start)
    brace = source.index("{", begin)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[begin:end]


def main() -> None:
    storage = (ROOT / "src/user/services/storage/src/service.rs").read_text()
    bootstrap = (ROOT / "src/user/services/core/src/service_manager/bootstrap.rs").read_text()
    matcher = item(storage, "unsafe fn mount_by_uuid(")
    selector = item(bootstrap, "let mut take_format =") + ";"
    fixture = r'''
use std::marker::PhantomData;
struct ChannelBlockDevice;
struct LiberFs<T> { uuid: [u8; 16], device: PhantomData<T> }
impl<T> LiberFs<T> { fn uuid(&self) -> [u8; 16] { self.uuid } }
#[derive(Clone, Copy)]
enum RootMountError { Missing, Ambiguous }
unsafe fn mount_system_volume(channel: u64) -> Option<LiberFs<ChannelBlockDevice>> {
    let id = match channel { 11 | 13 => 1, 12 | 22 => 2, _ => return None };
    Some(LiberFs { uuid: [id; 16], device: PhantomData })
}
'''
    checks = r'''
fn roles(mut role_blocks: Vec<u64>, block_formats: Vec<u8>) -> [u64; 3] {
    SELECTOR
    [take_format(4), take_format(2), take_format(3)]
}
fn main() {
    // Provider zero can be FAT while the loader-selected root is a later provider.
    assert_eq!(roles(vec![100, 200, 300, 400], vec![4, 1, 2, 3]), [100, 300, 400]);
    // Unknown media must not hide a fifth provider or be assigned by position.
    assert_eq!(roles(vec![100, 200, 300, 400, 500], vec![1, 0, 4, 2, 3]), [300, 400, 500]);
    // Embedded and block roots use the same table: the adverse media order still resolves.
    assert_eq!(roles(vec![100, 200, 300, 400], vec![1, 3, 2, 4]), [400, 300, 200]);
    assert_eq!(roles(vec![100, 200], vec![1, 0]), [0, 0, 0]);
    unsafe {
        assert_eq!(mount_by_uuid(11, &[11, 12], Some([2; 16])).ok().unwrap().1, 12);
        assert_eq!(mount_by_uuid(11, &[0, 0, 0, 0, 12], Some([2; 16])).ok().unwrap().1, 12);
        // The primary is already represented in probes; it must not count twice.
        assert_eq!(mount_by_uuid(11, &[11, 12], Some([1; 16])).ok().unwrap().1, 11);
        assert!(matches!(mount_by_uuid(11, &[11, 13], Some([1; 16])), Err(RootMountError::Ambiguous)));
        assert!(matches!(mount_by_uuid(11, &[11, 12], Some([3; 16])), Err(RootMountError::Missing)));
        assert_eq!(mount_by_uuid(11, &[], Some([1; 16])).ok().unwrap().1, 11);
    }
}
'''.replace("SELECTOR", selector)
    source = fixture + matcher + checks
    rustc = os.environ.get("RUSTC", "rustc")
    with tempfile.TemporaryDirectory(prefix="provider-routing-") as folder:
        rust = Path(folder) / "main.rs"
        binary = Path(folder) / "routing"
        for label, body, should_pass in [
            ("production", source, True),
            ("duplicate-identity mutation", source.replace("if selected.is_some()", "if false"), False),
            ("provider-zero mutation", source.replace("role_blocks.get_mut(at)", "role_blocks.get_mut(at.max(1))"), False),
        ]:
            rust.write_text(body)
            built = subprocess.run([rustc, "--edition=2024", str(rust), "-o", str(binary)], capture_output=True, text=True)
            if built.returncode:
                raise SystemExit(f"provider-routing: {label} did not compile:\n{built.stderr}")
            ran = subprocess.run([str(binary)], capture_output=True, text=True)
            if (ran.returncode == 0) != should_pass:
                raise SystemExit(f"provider-routing: {label} gave the wrong verdict:\n{ran.stderr}")
    print("provider-routing: non-first and fifth roots, all media roles, missing/ambiguous identities passed; both regression mutations failed")


if __name__ == "__main__":
    main()
