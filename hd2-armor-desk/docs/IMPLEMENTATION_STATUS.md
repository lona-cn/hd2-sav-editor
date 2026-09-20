# 实施状态

本文件区分**已做**、**未做**、**平台验证**、**已知问题**与**下一动作**。
勾选只基于实际运行结果；未运行的写 NOT RUN，不用推断代替。

构建与运行环境：

| 项目 | 值 |
|---|---|
| 机器 | Intel(R) Core(TM) Ultra 5 250K Plus |
| 系统 | Windows 11 Pro 10.0.26200 x64 |
| 工具链 | `stable`，由 `rust-toolchain.toml` 固定（已验证 stable 下 110 个测试全通过） |
| 依赖 | `Cargo.lock` 锁定；`gpui-kit 0.6.4` / `gpui-pre 0.3.5` / `gpui-component 0.6.4` |
| 构建 | `cargo build --release --locked` → `target\release\hd2-armor-desk.exe` |

---

## 已做

### 代码

| 层 | 内容 |
|---|---|
| `sav_codec` | 头部/块表解析、raw LZ4 解压与重压、内层 MurmurHash64A-low32、外层 CRC32 IEEE、字节保真重打包、白名单差异校验 |
| `loadout_domain` | `Snapshot`/`Draft`/撤销栈、`LoadoutIntent`、目录（去重与类型判定）、旧 JSON/CSV/v2 JSON 导入、冲突与非法值报告、预设存取、差异预览 |
| `local_io` | 稳定读取器、只读监视、备份与 SHA-256、另存、安全写回、基于最新文件的风险写入、回读校验、恢复 |
| `app` | GPUI 界面：双槽卡片（身体默认解锁）、搜索、导入预览、预设、差异栏、双写回模式、诊断与后台任务 |

### 验收对照（自动测试）

| ID | 结果 | 证据 |
|---|---|---|
| A01 | PASS | `golden::hostile_inputs_are_rejected`、`golden::encoder_rejects_unsafe_requests`、`transactions::unknown_layout_cannot_be_prepared_for_writing` |
| A02 | PASS | `oracle` 套件 228 组向量（含 0 长度与 1..7 尾字节） |
| A03 | PASS | `golden::no_patch_repack_is_byte_identical`、`transactions::no_op_commit_is_byte_identical` |
| A04 | PASS | `golden::patched_repack_changes_only_allowed_ranges`（断言所有差异落在允许范围，不硬断言字节数） |
| A05 | PASS | 同上（body 路径） |
| A06 | PASS | 同上（双槽同时设置） |
| A07 | PASS | `oracle` 套件：Rust 输出可被 Python 参考实现读取 |
| A08 | PASS | `golden::block_reuse_and_cross_block_hash_refresh`、`transactions::later_block_edit_refreshes_the_inner_hash` |
| A09 | PASS | `golden::inner_hash_is_mandatory_and_covers_whole_payload` |
| A10 | PASS | `golden::patched_repack_changes_only_allowed_ranges` |
| A11 | PASS | `golden::unaligned_reads_are_safe` |
| A12 | PASS | `golden::hostile_inputs_are_rejected`（截断/非零 padding/多余尾部/恶意长度） |
| B01 | PASS | `import::legacy_json_example_imports_without_column_juggling`、`import::real_workspace_catalog_imports_when_present`（**真实 98 条目录，未跳过**） |
| B02 | PASS | `import::csv_with_bom_quotes_commas_and_newlines_parses` |
| B03 | PASS | `import::contradictory_hex_and_decimal_columns_are_reported`、`import::csv_out_of_range_id_is_reported_with_line_number`、`import::large_ids_are_not_truncated_or_made_negative` |
| B04 | PASS | `import::same_named_helmet_and_armor_stay_separate_items` |
| B05 | PASS | `import::same_armor_at_head_and_body_is_one_item_with_two_observations` |
| B06 | PASS | `import::head_observation_alone_does_not_imply_helmet`、`import::confirming_type_creates_a_distinct_entry` |
| B07 | PASS | `import::reimport_is_idempotent_and_preserves_manual_names`、`import::import_does_not_modify_source_bytes` |
| B08 | PASS | `import::v2_document_with_inconsistent_key_is_rejected`、`import::negative_and_oversized_json_values_are_rejected` |
| B09 | PASS | `import::items_without_metadata_remain_usable` |
| C01 | PASS | `ui_flow::opening_a_save_populates_both_slot_cards`（两卡片实际布局）+ release 截图 |
| C03 | PASS | `ui_flow::body_starts_unlocked_and_can_be_locked_without_writing` |
| C11 | PASS | `ui_flow::an_empty_catalog_never_claims_a_type_for_the_head_slot`（空目录仍可打开） |
| D01 | PASS | `monitor::half_written_file_keeps_last_good_and_then_publishes` |
| D02 | PASS | `monitor::rename_replace_is_followed_without_holding_a_handle` |
| D05 | PASS | `monitor::switching_documents_bumps_generation_and_old_events_are_identifiable` |
| E01 | PASS | `transactions::save_copy_refuses_an_existing_target` |
| E02 | PASS | `transactions::save_copy_refuses_to_target_the_source_itself` |
| E03 | PASS | 安全模式拒绝过期基线；风险模式由 `transactions::force_latest_*` 与 `ui_flow::risk_write_rebases_*` 覆盖 |
| E04 | PASS | `transactions::restore_refuses_a_corrupt_backup` |
| E07 | PASS | `transactions::commit_writes_back_with_a_verified_backup`（含回读） |
| E09 | PASS | `transactions::second_commit_on_a_stale_base_is_refused`、`ui_flow::a_clean_draft_keeps_write_back_disabled` |
| E10 | PASS | `transactions::restore_replaces_everything_and_keeps_a_safety_copy`；界面入口见 `ui_flow::restore_is_reachable_and_whole_file_scoped` |
| 监视只读 | PASS | `monitor::monitoring_never_writes_to_the_watched_file`、`ui_flow::the_watcher_toggle_flips_and_never_touches_the_file` |

另外，恢复功能已接入界面：工具栏「恢复整份备份」列出 `backups\` 中的备份，
逐条「恢复这一份」；它与卡片上「恢复普通头盔」（仅头部槽位）是范围不同的两个操作。
`ui_flow::restore_is_reachable_and_whole_file_scoped` 与
`ui_flow::recording_a_restore_rebuilds_the_document_from_disk` 覆盖该路径。

合计：**110 passed, 0 failed**（`cargo test --workspace --locked`，stable 工具链）。

### 平台验证

- Windows 11 x64：110 个测试通过；重建程序已实际启动并点击到双模式写回确认框。
- `cargo fmt --all -- --check` 通过；`cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- release 构建通过；Linux **未验证**（本项目为 Windows 目标）。

---

## 未做 / NOT RUN

| 项目 | 状态 | 原因 |
|---|---|---|
| 真实桌面最终写盘 | 部分 | 实际桌面已完成「打开 → 选护甲 → 打开双模式确认框」；未点击最终写入以避免修改仓库 fixture。隔离 scratch 文件上的完整写回由 `ui_flow::risk_write_rebases_*` 覆盖。 |
| C02 头部卡片显示类型 | PASS | 实际桌面确认名称、ID 与装备类型可见，内部证据分类不在装备库中显示。 |
| C04–C10、C12 | 部分 | 本轮实测槽位选择、默认解锁、差异栏和写回确认框；IME 与其余游戏内交互仍未验证。 |
| D03/D04 冲突与风险写入 | PASS | 安全模式拒绝过期基线；风险模式基于最新文件重建并保留未选择槽位，事务与真实 GPUI 点击均已测试。 |
| D06 Steam 多账号选择 | NOT RUN | 需要多账号环境。 |
| D07/D08 | NOT RUN | 需要真实游戏写入配合。 |
| E05/E06 权限/长路径/磁盘满 | NOT RUN | 未做故障注入实测。 |
| E08/E11/E12 | 部分 | 备份与安全副本已测（含恢复前副本 `before_restore_*`）；崩溃现场保留与 SteamCloud 行为未实测。 |
| F01 无 Rust/Python 环境运行 | 部分 | release 程序本身不依赖 Python/Rust，但未在纯净机器上验证。 |
| F07 日志轮转 | NOT RUN | 未实现独立日志轮转（诊断信息按需展开，不落盘）。 |
| F08 图片/压缩包导入 | NOT RUN | 未实现图片导入，因此无此攻击面。 |
| G01–G06 游戏内验收 | NOT RUN | 需用户账号实测。 |
| 真实 `.sav` 读写 | NOT RUN | 包内不含真实存档；需用户本地选择。 |

---

## 已知问题

1. **与游戏并发写入仍有竞争**：风险模式会基于最新有效文件重建，但游戏可能在之后再次覆盖结果。
2. **真实桌面最终写盘未执行**：桌面已实际点到确认框；最终写盘在隔离 scratch 的 GPUI 测试中完成。
3. ~~nightly 工具链~~：已改为 `rust-toolchain.toml` 固定 stable，并在 stable 下复跑全部测试通过。
4. **未知布局只读**：结构不符的存档会以只读打开并给出理由，这是有意行为。

---

## 下一动作

1. 在非仓库 fixture 上补做真实桌面最终写盘冒烟。
2. ~~验证稳定工具链并固定~~：已完成，见 `rust-toolchain.toml`。
3. 按 README 第 8 节顺序完成 G01–G06 游戏内验收。
4. 按需补做 D06 多账号选择的端到端验证。
