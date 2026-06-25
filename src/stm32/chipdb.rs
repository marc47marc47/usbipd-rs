use crate::*;

/// A chip definition parsed from a `.chip` file. Only the fields this tool uses
/// are kept.
pub(crate) struct ChipDef {
    pub(crate) dev_id: u16,
    pub(crate) name: String,
    pub(crate) flash_size_addr: Option<u32>,
    pub(crate) sram_kb: Option<u32>,
    pub(crate) source: String,
}

/// Parse a C-style integer as written in `.chip` files: `0x...` hex or decimal.
pub(crate) fn parse_chip_int(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches([',', ';']);
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Parse one `.chip` file's text. Returns `None` if it has no `chip_id`.
pub(crate) fn parse_chip_file(text: &str) -> Option<ChipDef> {
    let mut dev_id = None;
    let mut name = None;
    let mut flash_size_addr = None;
    let mut sram_bytes = None;
    for raw in text.lines() {
        // Strip `// ...` and `# ...` comments, then split key/value.
        let line = raw.split("//").next().unwrap_or(raw);
        let line = line.split('#').next().unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let (key, val) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        match key {
            "chip_id" => dev_id = parse_chip_int(val).map(|v| v as u16),
            "dev_type" => name = Some(val.replace('_', " ")),
            "flash_size_reg" => flash_size_addr = parse_chip_int(val),
            "sram_size" => sram_bytes = parse_chip_int(val),
            _ => {}
        }
    }
    let dev_id = dev_id?;
    Some(ChipDef {
        dev_id,
        name: name.unwrap_or_else(|| format!("STM32 (chip_id 0x{dev_id:03X})")),
        flash_size_addr,
        sram_kb: sram_bytes.map(|b| b / 1024),
        source: String::new(),
    })
}

/// Directories searched for `.chip` files: `etc/chips/` relative to the working
/// directory (running from a checkout) and `etc/chips` / `chips` next to the
/// executable (running a packaged binary).
pub(crate) fn chip_search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("etc/chips")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("etc/chips"));
            dirs.push(dir.join("chips"));
        }
    }
    dirs
}

/// Load every readable `.chip` file from the search directories.
pub(crate) fn load_chip_db() -> Vec<ChipDef> {
    let mut defs = Vec::new();
    for dir in chip_search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("chip") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(mut def) = parse_chip_file(&text) {
                    def.source = path.display().to_string();
                    defs.push(def);
                }
            }
        }
    }
    defs
}

/// Resolved chip parameters used by the native reader: `.chip` overrides the
/// built-in name / flash address / SRAM; the (verified) UID and RDP register
/// addresses always come from the built-in family table.
pub(crate) struct ResolvedChip {
    pub(crate) name: String,
    pub(crate) flash_size_addr: Option<u32>,
    pub(crate) uid_addr: Option<u32>,
    pub(crate) rdp_addr: Option<u32>,
    pub(crate) rdp_kind: Option<RdpKind>,
    pub(crate) sram_kb: Option<u32>,
    pub(crate) source: String,
}

/// Authoritative main-SRAM sizes (KB) for built-in families, taken from the
/// stlink chip database. Used when no `.chip` file supplies one.
pub(crate) fn builtin_sram_kb(dev_id: u16) -> Option<u32> {
    Some(match dev_id {
        0x412 => 10,
        0x410 => 20,
        0x414 | 0x418 => 64,
        0x411 => 128,
        0x423 => 64,
        0x433 => 96,
        0x413 => 192,
        0x419 => 256,
        0x431 | 0x421 => 128,
        0x441 => 256,
        0x449 => 320,
        0x451 => 512,
        0x452 => 256,
        _ => return None,
    })
}

/// Resolve a DBGMCU DEV_ID into chip parameters, preferring a matching `.chip`
/// file over the built-in table. Returns `None` if neither knows the model.
pub(crate) fn resolve_chip(dev_id: u16, db: &[ChipDef]) -> Option<ResolvedChip> {
    let builtin = stm_family(dev_id);
    match db.iter().find(|c| c.dev_id == dev_id) {
        Some(chip) => Some(ResolvedChip {
            name: chip.name.clone(),
            flash_size_addr: chip.flash_size_addr.or(builtin.map(|f| f.flash_size_addr)),
            uid_addr: builtin.map(|f| f.uid_addr),
            rdp_addr: builtin.map(|f| f.rdp_addr),
            rdp_kind: builtin.map(|f| f.rdp_kind),
            sram_kb: chip.sram_kb.or_else(|| builtin_sram_kb(dev_id)),
            source: chip.source.clone(),
        }),
        None => builtin.map(|f| ResolvedChip {
            name: f.name.to_string(),
            flash_size_addr: Some(f.flash_size_addr),
            uid_addr: Some(f.uid_addr),
            rdp_addr: Some(f.rdp_addr),
            rdp_kind: Some(f.rdp_kind),
            sram_kb: builtin_sram_kb(dev_id),
            source: "built-in".to_string(),
        }),
    }
}


#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn chip_file_parses_stlink_format() {
        let text = "\
# comment line
dev_type STM32F446
chip_id 0x421                // STM32_CHIPID_F446
flash_type F2_F4
flash_size_reg 0x1fff7a22
sram_size 0x20000            // 128 KB
option_base 0x40023c14
";
        let def = parse_chip_file(text).expect("has chip_id");
        assert_eq!(def.dev_id, 0x421);
        assert_eq!(def.name, "STM32F446");
        assert_eq!(def.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(def.sram_kb, Some(128));
        assert!(parse_chip_file("dev_type Foo\n").is_none()); // no chip_id
    }

    #[test]
    fn chip_int_accepts_hex_and_decimal() {
        assert_eq!(parse_chip_int("0x20000"), Some(0x20000));
        assert_eq!(parse_chip_int("0X10"), Some(16));
        assert_eq!(parse_chip_int("512"), Some(512));
        assert_eq!(parse_chip_int("0x421,"), Some(0x421)); // trailing punctuation
    }

    #[test]
    fn resolve_chip_falls_back_to_builtin_when_no_file() {
        let r = resolve_chip(0x421, &[]).expect("F446 is built-in");
        assert!(r.name.contains("STM32F446"));
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // verified built-in UID addr
        assert_eq!(r.sram_kb, Some(128));
        assert_eq!(r.source, "built-in");
    }

    #[test]
    fn resolve_chip_file_overrides_builtin_but_keeps_verified_addrs() {
        let db = vec![ChipDef {
            dev_id: 0x421,
            name: "My Custom F446 Board".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(128),
            source: "etc/chips/F446.chip".to_string(),
        }];
        let r = resolve_chip(0x421, &db).unwrap();
        assert_eq!(r.name, "My Custom F446 Board"); // file name wins
        assert_eq!(r.uid_addr, Some(0x1FFF7A10)); // UID still from built-in
        assert_eq!(r.source, "etc/chips/F446.chip");
    }

    #[test]
    fn resolve_chip_file_adds_a_part_unknown_to_builtin() {
        let db = vec![ChipDef {
            dev_id: 0x999,
            name: "STM32 Experimental".to_string(),
            flash_size_addr: Some(0x1FFF7A22),
            sram_kb: Some(64),
            source: "etc/chips/X.chip".to_string(),
        }];
        let r = resolve_chip(0x999, &db).expect("file-defined part resolves");
        assert_eq!(r.name, "STM32 Experimental");
        assert_eq!(r.flash_size_addr, Some(0x1FFF7A22));
        assert_eq!(r.sram_kb, Some(64));
        assert_eq!(r.uid_addr, None); // not in built-in → no UID/RDP
        assert_eq!(r.rdp_addr, None);
        assert!(resolve_chip(0x999, &[]).is_none()); // unknown without a file
    }
}
