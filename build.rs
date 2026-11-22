use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod app_icon {
    include!("src/icon.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR missing"));
    let ico_bytes = build_multi_res_ico(&[16, 32, 48, 64, 128, 256]);

    let out_ico = out_dir.join("app.ico");
    fs::write(&out_ico, &ico_bytes).expect("failed to write OUT_DIR/app.ico");

    let pkg_ver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.6.0".to_string());
    let ver_major = env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_else(|_| "0".to_string());
    let ver_minor = env::var("CARGO_PKG_VERSION_MINOR").unwrap_or_else(|_| "6".to_string());
    let ver_patch = env::var("CARGO_PKG_VERSION_PATCH").unwrap_or_else(|_| "0".to_string());

    let rc_content = format!(
        r#"1 ICON "app.ico"

1 VERSIONINFO
FILEVERSION {ver_major},{ver_minor},{ver_patch},0
PRODUCTVERSION {ver_major},{ver_minor},{ver_patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "FileDescription", "Markdown Studio\0"
            VALUE "FileVersion", "{pkg_ver}.0\0"
            VALUE "InternalName", "markdown_studio\0"
            VALUE "OriginalFilename", "Markdown Studio.exe\0"
            VALUE "ProductName", "Markdown Studio\0"
            VALUE "ProductVersion", "{pkg_ver}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );

    let rc_path = out_dir.join("app.rc");
    let res_path = out_dir.join("app.res");
    fs::write(&rc_path, rc_content).expect("failed to write app.rc");

    if let Some(rc_exe) = find_windows_rc_exe() {
        let status = Command::new(&rc_exe)
            .args([
                "/nologo",
                "/fo",
                res_path.to_str().expect("non-utf8 res path"),
                rc_path.to_str().expect("non-utf8 rc path"),
            ])
            .status();

        match status {
            Ok(s) if s.success() && res_path.exists() => {
                println!("cargo:rustc-link-arg-bins={}", res_path.display());
            }
            Ok(s) => {
                println!("cargo:warning=rc.exe exited with status {s}; exe icon not embedded");
            }
            Err(e) => {
                println!("cargo:warning=failed to run rc.exe ({e}); exe icon not embedded");
            }
        }
    } else {
        println!("cargo:warning=Windows SDK rc.exe not found; exe icon not embedded");
    }
}

fn find_windows_rc_exe() -> Option<PathBuf> {
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let kits_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if let Ok(entries) = fs::read_dir(kits_bin) {
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("x64").join("rc.exe"))
            .filter(|p| p.is_file())
            .collect();
        versions.sort();
        if let Some(latest) = versions.pop() {
            return Some(latest);
        }
    }
    None
}

fn build_multi_res_ico(sizes: &[usize]) -> Vec<u8> {
    let mut images: Vec<(usize, Vec<u8>)> = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        let rgba = app_icon::render_app_icon_rgba(sz);
        let dib = encode_bmp_dib_32(sz, &rgba);
        images.push((sz, dib));
    }

    let count = images.len() as u16;
    let mut ico = Vec::new();
    // ICONDIR
    ico.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Type = 1 (ICON)
    ico.extend_from_slice(&count.to_le_bytes());

    let mut offset = 6usize + images.len() * 16;
    for (sz, dib) in &images {
        let dim = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(dim); // bWidth
        ico.push(dim); // bHeight
        ico.push(0); // bColorCount
        ico.push(0); // bReserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
        ico.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes()); // dwBytesInRes
        ico.extend_from_slice(&(offset as u32).to_le_bytes()); // dwImageOffset
        offset += dib.len();
    }

    for (_, dib) in images {
        ico.extend_from_slice(&dib);
    }
    ico
}

fn encode_bmp_dib_32(size: usize, rgba: &[u8]) -> Vec<u8> {
    let and_stride = size.div_ceil(32) * 4;
    let and_size = and_stride * size;
    let xor_size = size * size * 4;

    let mut dib = Vec::with_capacity(40 + xor_size + and_size);
    // BITMAPINFOHEADER
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight (XOR + AND)
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // Bottom-up BGRA XOR bitmap
    for y in (0..size).rev() {
        for x in 0..size {
            let idx = (y * size + x) * 4;
            dib.push(rgba[idx + 2]); // B
            dib.push(rgba[idx + 1]); // G
            dib.push(rgba[idx]); // R
            dib.push(rgba[idx + 3]); // A
        }
    }

    // Bottom-up 1bpp AND mask
    for y in (0..size).rev() {
        let mut row = vec![0u8; and_stride];
        for x in 0..size {
            let alpha = rgba[(y * size + x) * 4 + 3];
            if alpha == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        dib.extend_from_slice(&row);
    }
    dib
}
