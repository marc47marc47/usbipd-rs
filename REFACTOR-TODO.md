# 重構進度追蹤 — 分拆模組 + 完整型別化

> 計畫全文：`C:\Users\marc4\.claude\plans\polymorphic-plotting-pony.md`
> 目標：`src/bin/usbipd-rs.rs`（5306 行）→ lib.rs + 多個 <500 行模組，並把
> `HashMap<String,String>` 資料流完整型別化。**行為不得改變**（純重構）。
> 規則：每個項目完成後 `cargo build --release` 必須**警告乾淨**且 `cargo test` 全綠。
> （注意：另一個 `TODO.md` 是既有功能追蹤，與本檔無關，勿混用。）

## 狀態圖例
- [ ] 待辦  · [~] 進行中  · [x] 完成  · [!] 受阻
- `併` = 可與同階段其他項目並行（不同檔案，無共寫衝突）
- `序` = 必須序列（會動到共同/尚未分拆的檔案）

---

## Phase 0 — 基準擷取（序，先做）
- [x] 0.1 `序` 擷取重構前基準輸出存檔，供回歸比對：`--help`、`--list-tools`、`--list`、`--probe`（已接 CMSIS-DAP probe）→ scratchpad/baseline/
- [x] 0.2 `序` 確認 `cargo build --release` 與 `cargo test` 在重構前為綠（22 測試通過、警告乾淨）

## Phase 1 — 骨架（序，阻擋後續全部）— ✅ 完成
- [x] 1.1 `序` `src/lib.rs`：crate 根（含 `#![cfg_attr(not(windows), allow(dead_code))]`、`pub(crate) use` 外部依賴、mod 宣告、`run()`）
- [x] 1.2 `序` `src/bin/usbipd-rs.rs` 縮成 `fn main() { usbipd_rs::run() }`
- [x] 1.3 `序` lib+bin 可編譯、22 測試通過、輸出與基準一致

## Phase 2 — 機械式分拆 — ✅ 完成（所有檔 <500 行，最大 swd/cmsisdap.rs 488）
> 以行區間分桶切割，欄位0頂層項目自動補 `pub(crate)`，跨模組欄位/方法可見性以編譯器逐一補齊。
- [x] 2.1 `序` `usb.rs`（HEADERS、print_table、pad_display、Entry、nusb 列舉、cmd_list_usb）
- [x] 2.2 `序` `boards.rs`（AvrTarget、ProbeKind、KnownBoard、KNOWN_BOARDS、pipeline 常數、lookup_board）
- [x] 2.3 `序` `cli.rs`（print_help；parse 留待 Phase 4）
- [x] 2.4 `序` `probe/mod.rs`（probe_boards 編排、安裝建議；run/print 仍為 match）
- [x] 2.5 `序` probe 子模組：esp/avr/stm32flash/ftdi/dfu/pico/daplink(含 pyocd)
- [x] 2.6 `序` `stm32/`（mod 含 gd32、chipdb、decode）
- [x] 2.7 `序` `stlink/`（controller、driver、target）
- [x] 2.8 `序` `swd/`（mod 共用、stlink、cmsisdap）
- [x] 2.9 `序` `windows/`（mod、status）
- [x] 2.10 `序` `installer/`（mod、tools 表）+ `cmd/mcu_alive.rs`
- [x] 2.11 `序` 測試：暫保留於 `lib.rs` root（`mod tests`，super::* 解析 crate re-export；被測私有欄位/方法已補 pub(crate)），22 測試全綠。**偏離計畫**：未逐一拆入各模組，留待後續可選。

## Phase 3 — 型別化各報告（分拆後多檔，部分可並行）
> 每項：新增報告 struct → `run_*` 回傳它 → `parse_*` 填它 → `print_*` 吃它。逐項驗證輸出不變。
- [ ] 3.1 `序` `stm32/decode.rs`：`TargetReport` 取代 decode_stlink_regs 的 HashMap（含測試遷移）— 收益最大、先做
- [ ] 3.2 `併` `probe/esp.rs`：`EspInfo`
- [ ] 3.3 `併` `probe/avr.rs`：`AvrInfo`
- [ ] 3.4 `併` `probe/stm32flash.rs`：`Stm32FlashInfo`
- [ ] 3.5 `併` `probe/ftdi.rs`：`FtdiInfo`
- [ ] 3.6 `併` `probe/dfu.rs`：`DfuInfo`（含 region 列）
- [ ] 3.7 `併` `probe/pico.rs`：`PicoInfo`
- [ ] 3.8 `併` `probe/daplink.rs`：`DaplinkInfo` + `PyocdInfo`
- [ ] 3.9 `併` `stlink/controller.rs`：`StlinkControllerInfo`

## Phase 4 — 統一派發與 CLI 型別化（序，動 probe/mod 與 cli）
- [ ] 4.1 `序` `ProbeReport` enum + `ProbeCtx`；`ProbeKind::run` / `ProbeReport::print` 收掉 probe_boards 巨型 match
- [ ] 4.2 `序` `ProbeKind::label` 收掉 `cmd_list_usb` 的 label match；`flasher_suggestion` 一併收斂
- [ ] 4.3 `序` `cli.rs`：`Command` enum + `parse(args)`；`lib.rs::run()` 改 match `Command`

## Phase 5 — SWD 抽象（序）
- [ ] 5.1 `序` `swd/mod.rs`：`TargetLink` trait（read_mem32 等）；`StlinkLink`/`CmsisDapLink` 各 impl，共用 collect/format

## Phase 6 — 收尾（序）
- [ ] 6.1 `序` 更新 `CLAUDE.md`：「單一檔案」段落改為新模組地圖與擴充指引
- [ ] 6.2 `序` 對齊 `README` / `--help`（若受影響）
- [ ] 6.3 `序` 最終 `cargo build --release`（警告乾淨）+ `cargo test` + 與 Phase 0 基準逐項比對
- [ ] 6.4 `序` `bash pack-release.sh` 確認打包流程仍動

---

## 多 AI 協作建議
- Phase 0/1/2 必須**序列**（動到尚未分拆的單一大檔）→ 由單一 agent 完成。
- Phase 3 的 `併` 項目在 Phase 2 完成後可分派給多個 agent（各自獨立檔案；建議各自 worktree 隔離避免共寫）。
- Phase 4/5/6 需序列（動到共用的 `probe/mod.rs`、`cli.rs`、`swd/mod.rs`、文件）。

## 設計決策（Phase 3 之前釐清）
- **Target 報告型別** = 單一 `TargetReport`（Option 具名欄位）+ 固定順序 `print` 方法。
  探針列（probe/dp_idcode/layer2/fix/hint）與 layer-2 解碼列（device_id/revision/vendor/core/
  architecture/max_clock/flash_size/flash_map/sram/unique_id/read_protection/identification/
  transport/access/source）同為該 struct 的 Option 欄位。
- **STM32 與 RP2040 target 共用同一組可印出鍵**（RP2040 僅填子集），故**單一 struct 即可**，
  不需 enum 變體。但須精準保留現況差異：`format_target_rows` 會丟棄 `identification`（STM32 路徑
  不印），`rp2040_rows` 則會設定 `identification`。`transport` 在 native 路徑覆寫為
  "SWD (native nusb, read-only)"。print 的縮排/欄寬（探針列 2 空格 `{:<16}`、layer-2 列 4 空格、
  no-target 分支硬寫 `"  Layer 2:         "`）必須逐字保留。

## ⚠️ 重要：Phase 3 與 Phase 4 必須合併執行
`probe_boards` 目前把所有 probe 統一當 `Result<HashMap<String,String>>` 派發。若把個別 probe 改回傳
具名 struct，統一派發會中斷。因此 **Phase 4.1（ProbeReport enum + ProbeKind::run/ProbeReport::print）
要先（或同時）建立**，才能讓各 probe 回傳自己的型別。修正後的順序：
1. 先建 `ProbeReport` enum 骨架 + `ProbeKind::run`/`ProbeReport::print`（原 4.1），各變體先包現有 HashMap。
2. 再逐一把變體的 payload 從 HashMap 換成具名 struct（原 3.1~3.9），每換一個比對輸出。
3. 再做 CLI `Command` enum（4.3）與 SWD `TargetLink` trait（5）。

## 進度筆記
- Phase 0-2 完成並驗證：5306 行單檔 → lib.rs(358) + 25 模組(全<500)；`-D warnings` 乾淨；
  22 測試通過；`--help`/`--list-tools`/`--list`/`--probe`(實機) 輸出逐字 IDENTICAL。
- Phase 3-6 尚未開始（型別化）；已釐清資料流與設計，待續（合併執行 3+4）。
- 尚未 commit（等使用者指示）。
