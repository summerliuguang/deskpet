# DeskPet 优化改进方案 v2

基于 2026-09 全量代码审查 + 真机反馈（拖动不跟手）整理。所有改动遵循既有纪律：
**零 GPU、事件驱动按需唤醒、不引入新框架/运行时**；每项可独立验证、独立回滚。

问题编号：B* = 真 Bug，A* = 架构债，R* = 资源/健壮性，S* = 安全/打磨，T* = 测试。

---

## 第 1 批：真 Bug 修复（P0，发布 v0.3.3）

### B1 拖动半速跟随（用户真机已复现）

**问题**：`main.rs` CursorMoved 拖动分支用 `cursor_local - grab` 计算新窗口位置，
`grab` 是拖动开始时的快照（本地坐标 − 当时窗口全局位置）。但 CursorMoved 给的是
**窗口本地坐标**，其原点随窗口移动而平移；固定锚点解移动坐标系，稳态下
窗口位移 = 鼠标位移的一半，宠物越拖越掉队（走查数据见下）。

| 事件 | 鼠标全局X | 窗口X | 本地X | 公式 `100+(本地-30)` | 应到 |
|---|---|---|---|---|---|
| A | 140 | 100 | 40 | 110 | 110 ✓ |
| B | 150 | 110 | 40 | 110（冻结） | 120 ✗ |
| C | 160 | 110 | 50 | 120 | 130 ✗ |

**改法**：增量式。`begin_drag` 记 `drag_last = cursor`（本地）；每次 CursorMoved：
```rust
let (dx, dy) = (self.cursor.0 - self.drag_last.0, self.cursor.1 - self.drag_last.1);
self.pos = ((self.pos.0 as f64 + dx) as i32, (self.pos.1 as f64 + dy) as i32); // 再 clamp
self.drag_last = self.cursor;
```
本地坐标系内的差值即真实鼠标位移，与窗口挪到哪无关；捕获期间坐标为负也正确。
`drag_track`/甩飞速度不变。

**验证**：抽出纯函数 `apply_drag(pos, last_local, cur_local) -> (pos, last)`，
单测模拟 5 步连续移动断言窗口位置 == 鼠标全程位移（回归守护半速 bug）。
**回滚**：git revert 单提交。**工作量**：0.5h 代码 + 0.5h 测试。

### B2 双击误判（点一下再拖 → 睡觉）

**问题**：`on_left_press` 的双击判定锚点是"距上次**按下** 400ms"。单击摸头
（~150ms 松开）后紧接着按住想拖动，落在 400ms 窗口内 → 误判双击 → 睡觉。

**改法**：锚点改为"距上次**松开**"：`last_release: Option<(Instant, (f64,f64))>`，
在 `on_left_release` 的 click 分支记录；按下时用它判定。
**验证**：抽出 `is_double_click(now, last_release, cur, prev_pos)` 纯函数单测
（按下-松开-快速按住 ≠ 双击）。
**工作量**：0.5h。

### B3 UI_SCALE OnceLock 失效 → 跨 DPI 屏拖动不缩放

**问题**：`lib.rs` 的 `UI_SCALE: OnceLock<f64>` 只能 set 一次，
`ScaleFactorChanged` 里的 `set_ui_scale` 第二次起被静默吞掉——缩放永远锁死在
首次启动值。且 `PixelPet.sprite_scale` 在构造时定死，`ScaleFactorChanged` 里
`pet_size = model.size()` 拿到的是没变的旧值，surface resize 形同虚设。
主屏 100% + 副屏 150% 场景完全失效。

**改法**：
```rust
static UI_SCALE: AtomicU32 = AtomicU32::new(256); // 定点 ×256
pub fn set_ui_scale(s: f64) { UI_SCALE.store((s.max(0.5)*256.0).round() as u32, Relaxed); }
pub fn ui_scale() -> f64 { UI_SCALE.load(Relaxed) as f64 / 256.0 }
```
`ScaleFactorChanged` 中重建模型（内置模型 `make_model(kind)` 重算 sprite_scale；
帧序列模型固定 64px 不受 DPI 影响，维持现状并在 README 注明）。气泡/输入框/待办
重建逻辑已有，保留。**顺带修**：`text.rs::base_px()` 每次 read，无需改。
**验证**：单测 set 两次值变化；真机 100%↔150% 双屏拖动，宠物窗口边长整数倍变化。
**工作量**：1h + 真机验证。

### B4 帧序列模型爬墙不旋转（视觉 bug）

**问题**：`model.rs` 里只有 `PixelPet` 在 Climb 时 `rotate90_into`；
`sprite_model.rs::clip_name` 把 Climb 映射到 "walk" 直立播放——自制模型爬墙
姿态错误。且模型规范没有爬墙帧约定。

**改法**（两层）：
1. 通用兜底：`SpriteModel::render` 在 `pose.state == Climb && pose.aux != 0` 时
   对输出帧调用 `sprites::rotate90_into`（函数本身与 64×64 无关，直接复用）；
2. 规范扩展：`model.toml` 支持 `climb` 片段（预旋转帧），有则优先、无则旋转兜底。
   文档同步 `sprite_model.rs` 顶部注释与 README。
**验证**：单测——示例猫模型 Climb+aux=-1 渲染结果 != 直立帧。
**工作量**：1h。

---

## 第 2 批：资源与稳定性（P1，发布 v0.4.0）

### R1 SpriteModel 内存预算 + 懒加载 + LRU

**问题**：`load()` 同步解码全部片段×全部换装常驻 HashMap（每帧 16KB）。
Live2D 预渲染几百帧 = 上百 MB，与"~10MB 内存"定位冲突；且在**主线程**同步读盘，
大模型右键切换卡死数秒。

**改法**：懒加载 + 字节预算：
- `clips` 改为缓存 `(片段名, 换装) -> Option<Vec<Vec<u32>>>`，首次 render 时
  从磁盘解码（单片段 ~8 帧，几毫秒级，不卡）；
- 解码总量预算 `MAX_DECODED_BYTES = 24MB`，超预算按 last-use LRU 逐出整片段，
  下次命中重新解码；
- `load()` 只读 model.toml（快），右键切换不再卡顿——**无需引入后台线程**。
**验证**：单测——临时目录造超预算模型，断言内存受控且渲染仍正确（逐出后再解码）；
真机切大模型观察任务管理器内存与切换耗时。
**回滚**：恢复全量加载。**工作量**：4h。

### R2 panic 策略：abort → unwind + 事件处理兜底

**问题**：`panic = "abort"` 下任何线程（AI/TTS/钩子）的小 panic = 整个进程闪退，
catch_unwind 无效，panic hook 只来得及写一行日志。

**改法**：
- release profile 改 `panic = "unwind"`（代价：unwind 表 +~150KB，10MB exe 可接受）；
- `window_event` / `user_event` / `about_to_wait` 的方法体包
  `catch_unwind(AssertUnwindSafe(...))`，panic 时 dlog 记录 + request_redraw 继续；
  工作线程 panic 从此只死线程不死进程；
- `verify_build.py` 不变（产物完整性另有指纹守护）。
**验证**：debug 构造一个只在特定菜单 id 触发的 panic，确认宠物存活并留日志。
**风险**：panic 后状态可能不一致——兜底只保证不闪退，日志定位靠 R3。
**工作量**：2h。

### R3 运行期错误日志（替代遍地 `let _ =`）

**问题**：配置写失败（磁盘满/只读）、surface present 失败、todos/chat_log 写失败
全部无声，出问题只能靠现象猜。

**改法**：`dlog` 扩展为带级别（INFO/WARN）+ 同键 60 秒限频；在
`persist_settings` 失败、`buffer_mut`/`present` 失败、todos/chat_log 写失败、
模型加载失败四处补 WARN。零新依赖。
**工作量**：1.5h。

### R4 `--selftest` 冒烟命令

**问题**：Windows 专属路径（钩子/托盘/热键/色键/四窗口创建渲染）只验编译不验行为，
真机验证全靠手点。

**改法**：main 支持 `--selftest`：创建全部窗口（隐藏态）→ 各渲染 1 帧 →
遍历全部 PetState 渲染 → 托盘/钩子/热键注册探活 → 写 `selftest OK` 到日志退出 0。
真机上一条命令完成脚本化冒烟。
**工作量**：2h。

---

## 第 3 批：架构重构（P1，行为不变，发布 v0.4.x）

### A1 TimerWheel：拆掉手写定时器队列

**问题**：App 有 11 个 `Option<Instant>` 定时字段；每加一个定时任务要改 4 处
（字段/构造/next_wakeup/about_to_wait），**漏加 next_wakeup = 事件循环睡死**，
是本项目最高频的结构性风险（autosave/due_check/onboard 都是最近这么加的）。

**改法**（最小实现，不是框架）：
```rust
enum TKind { Frame, Bubble, Reaction, Whisper, Typing, Hang, Drink, Sit,
             Autosave, DueCheck, Onboard, Think, Press }
struct Timer { kind: TKind, at: Instant, recur: Option<Duration> }
// BTreeMap<Instant, Vec<TKind>> 或小 Vec 扫描 min；fire(elapsed) 逐 kind 分发到现有逻辑
```
字段收敛为 `timers: Timers`；Autosave/DueCheck 用 `recur`。现有分发逻辑
（about_to_wait 里的各 if 块）原样搬进 `fire(kind)`，纯机械移动。
**验证**：单测——插入/触发顺序、recur 续期、cancel；全量行为冒烟。
**工作量**：4h。

### A2 状态 transition 统一化

**问题**：`enter_idle/enter_walk/enter_pose/enter_climb/enter_sleep_by_bubble/wake_with`
各自手工挑字段重置，漏一个就是"动画冻结"级 bug（历史上出过 mon_bottom 误递归、
批量替换事故）。

**改法**：抽 `PetCore { state, tick, state_len, frame_at, reaction_end, hang_until }`，
唯一入口 `transition(next, len)` **默认清全部瞬态计时**，调用方需要保留的
（如 Sleep 保留气泡）在 transition 后显式设置。PetCore 独立可单测。
**验证**：单测——任意 transition 后瞬态字段为空。
**工作量**：3h。

### A3 派生窗口统一路由（WindowManager）

**问题**：window_event 里四段 if-chain 逐窗口比对 id；隐藏恢复（set_hidden）、
缩放重建（ScaleFactorChanged）各自手写四份。

**改法**：
```rust
enum Derived { Menu(MenuWin), Input(InputBox), Todo(TodoWin), Bubble(BubbleWin) }
// HashMap<WindowId, Derived> 路由；hide_all/restore_all/scale_rebuild_all 各一份实现
```
**工作量**：3h。

### A4 像素 UI 原语层 + 文本参数收敛

**问题**：圆角判定/1px 描边/alpha 混色在 menu.rs 与 bubble.rs 各一份近似拷贝，
todo/inputbox 又一套边框画法；`text.rs::draw_clipped` 10 个参数坐标语义不统一
（x 有时是左缘有时 baseline）。

**改法**：`ui_draw.rs`：`rounded_panel(buf,w,h,rect,radius,bg,border)`、`hline`、
`blend_px`；`TextRow { x, y, clip, px, color, align }` 结构体参数替代位置参数。
先迁移 menu/bubble，再 todo/inputbox，视觉逐张对比（dump_frames 扩展导出四窗口帧）。
**工作量**：5h（含视觉回归对比）。

### A5 Settings 三连宏化

**问题**：每加一个开关改 struct/parse/persist 三处（已 13 字段），漏一处静默丢配置
（chat_log 就差点漏 persist）。

**改法**：`macro_rules! settings_fields` 一份清单生成三处代码。字段类型仅
bool/u64/Option<i64>/Option<usize>，宏复杂度可控。
**验证**：现有 parse 测试全过 + 新增 roundtrip 测试（parse(persist(x)) == x）。
**工作量**：2.5h。

---

## 第 4 批：安全与体验打磨（P2，发布 v0.5.0）

### S1 AI 错误分类重试

**问题**：现在 401（密钥无效）/404（模型不存在）也盲重试 2 次纯浪费；
报错原文直接甩气泡。

**改法**：`ai.rs::chat` 返回类型化错误 `enum ChatErr { Network, Http(u16,String),
Parse, Empty }`；只对 Network / 5xx / 429 重试；401→"密钥无效，检查 deskpet.toml"，
404→"模型不存在，检查 model 配置"。气泡文案人性化。
**工作量**：2h。

### S2 TLS TOFU 证书锁定

**问题**：`NoVerifier` 对 base_url 指向的任何服务器放弃 MITM 防护——配置文件被
篡改指向恶意服务器时 api_key 拱手送人。

**改法**：首次成功连接把服务端证书 SHA-256 存 exe 旁 `known_hosts` 文件；
自定义 verifier 比对指纹：无记录 → 写入（TOFU 安装）；不匹配 → 拒连 +
气泡告警"网关证书变更，非预期请检查 base_url"。
**新增依赖**：`sha2`（纯 Rust ~50KB，安全功能需要真哈希，论证成立）。
**工作量**：3h。

### S3 杂项打磨

- 互斥体 `Global\` → `Local\` 前缀（受限账户兼容）；
- `bubble_above` 的 `mon.y + 90` 等物理像素魔法数走 `ui(90)`；
- 菜单小屏适配：总高 > 屏高-16 时切紧凑度量（item_h 22/sep 6），仍溢出则截断
  （Set 页已 14 行，768p@125% 有风险）；
- `settings.model_kind`（注册表下标）改存**模型名**，增删模型后不再漂移
  （迁移：有旧下标无名字时按当前注册表解析一次写回名字）。
**工作量**：合计 3h。

### S4（可选）状态机时间基准：帧数 → 时长

**问题**：`state_len=60` 在 idle(600ms/帧)=36s、walk(180ms)=10.8s，掉帧时节奏
整体变慢；魔法数散落。**风险中等（改变所有行为节奏参数），可独立跳过**。
**改法**：`state_until: Option<Instant>`，进入状态时 `now + Duration`；tick 仅作
精灵动画相位。调参从"帧数"变"毫秒"，可读可测。
**工作量**：3h。

---

## 测试体系（贯穿各批）

- **T1** B1/B2 的坐标与双击纯函数回归测试；
- **T2** A1 TimerWheel、A2 PetCore::transition 单测；
- **T3** R1 预算逐出、B4 爬墙旋转、B3 ui_scale 二次 set 单测；
- **真机冒烟清单**（R4 落地后脚本化）：现有 14 项 + 拖动快速甩动跟随、
  双击后立即拖动、100%↔150% 跨屏拖、爬墙姿态、大模型切换不卡顿、
  断网聊天重试文案、拔副屏宠物迁移。

## 执行总表

| 批 | 内容 | 工作量 | 风险 | 发布 |
|---|---|---|---|---|
| 1 | B1-B4 真 Bug | ~0.5 天 | 低 | v0.3.3 |
| 2 | R1-R4 资源/稳定/自检 | ~1.5 天 | 中（R2） | v0.4.0 |
| 3 | A1-A5 架构重构（行为不变） | ~2.5 天 | 中 | v0.4.x |
| 4 | S1-S4 安全/打磨 | ~1.5 天 | 低 | v0.5.0 |

每批流程：实施 → cargo test 全绿 → windows-gnu 构建 + verify_build.py 指纹 →
真机冒烟 → 敏感扫描 → 单目的原子提交（不推送，等确认）。

## 明确不做（延续既定边界）

- 不引入 GPU 渲染 / tokio / egui·iced；
- 不做 AI 流式、配置热重载、版本迁移（前次评估已否决）；
- 不做帧序列模型的 DPI 整数倍放大（64px 固定是文档化取舍）；
- 不为重构而重构：A4 视觉原语迁移以"逐张对比无差异"为验收线。
