# deskpet-rs 项目报告

| 项 | 内容 |
|---|---|
| 项目名 | deskpet-rs（桌面小猫"团子"） |
| 版本 | v0.3.0 |
| 定位 | Windows 桌面宠物：轻量、低占用、AI 对话、可扩展模型 |
| 技术栈 | Rust 1.98 · winit 0.30 · softbuffer 0.4 · fontdue 0.9 · ureq/rustls · windows 0.62 · tray-icon 0.25 |
| 产物 | 单文件 `deskpet.exe`（10.1MB，Windows 10/11 x64，免安装） |
| 代码规模 | 3544 行 Rust（12 个源文件），12 个单元测试 |
| 许可/素材 | 内置 Unifont 字体（OFL/GPL+字体例外，全 CJK 覆盖）；像素猫为程序化绘制 |

---

## 一、项目概述

### 1.1 背景与目标

在调研了 GitHub 桌宠生态（desktop-pet 主题 1200+ 仓库）与主流方案（Electron / Tauri / PySide / WPF / Java Shimeji）后，确定为 **纯 Rust 路线**，核心诉求按优先级排序：

1. **零干扰**：不碰 GPU（游戏/视频渲染零竞争），不被反作弊误伤，全屏应用前台时自动隐藏
2. **轻量**：单文件、无运行时依赖、内存 ~10MB、空闲 CPU 趋近 0
3. **功能完整**：互动 + AI 对话 + 图片识别 + 语音 + 提醒 + 待办 + 联动
4. **可扩展**：模型抽象层（PetModel trait）支持换装换表情；形象方案定案为预设动作 + 帧序列模型，不引入 Live2D 运行时

### 1.2 交付形态

- `deskpet.exe`：拷贝即用，无控制台窗口、无后台服务、无自启动
- `deskpet.toml`（可选）：接入 AI/语音的配置，`deskpet.toml.example` 为模板

## 二、技术选型与调研结论

### 2.1 路线对比（调研结论）

| 方案 | 判定 | 理由 |
|---|---|---|
| Electron / Tauri | ❌ | WebView2 常驻内存 50-100MB+，与"几乎不影响其他程序"冲突 |
| PySide / WPF | ❌ | 依赖运行时或限于单一技术栈，非纯 Rust |
| **winit + softbuffer（纯 Rust 软件渲染）** | ✅ | 不创建任何 GPU/D3D/OpenGL 上下文；64×64 小窗软渲染成本可忽略；exe 极小 |

### 2.2 生态参考

交互范式参考了 Shimeji（爬墙/甩扔）、VPet（分区触摸/养成框架）、DyberPet（提醒/养成/AI 助手）、BongoCat（键鼠联动）；AI 桌宠趋势（监视编程代理、LLM 对话）纳入功能设计。

## 三、系统架构

### 3.1 模块图

```
                    ┌────────────────── 事件循环（winit，Wait/WaitUntil 按需唤醒）
                    │
 main.rs ─ App ─────┼── pet 窗口（64×64 透明置顶）←─ model.rs（PetModel trait）
                    │      │                          └─ sprites.rs（内置像素猫实现）
                    ├── bubble 窗（气泡，点击穿透）
                    ├── chat 窗（聊天，IME + 拖图识别）←─ text.rs（fontdue+Unifont 中文渲染）
                    ├── todo 窗（待办，todos.json 持久化）
                    ├── tray（tray-icon）+ 右键菜单（muda）
                    │
                    ├── ai.rs（OpenAI 兼容 + MiMo TTS，rustls 跳过自签证书）
                    ├── input.rs（键盘 LL 钩子 + XInput 轮询，独立线程）
                    ├── tts.rs（PlaySound SND_ASYNC）
                    └── config.rs（deskpet.toml，密钥只从配置/环境变量来）
```

### 3.2 核心抽象：PetModel trait（扩展接口）

```rust
pub trait PetModel: Send {
    fn render(&mut self, pose: &Pose) -> Vec<u32>;  // 状态+帧+目光+表情+皮肤 → ARGB 像素
    fn info(&self) -> &ModelInfo;                    // 皮肤/表情清单（驱动右键菜单）
    fn set_costume(&mut self, idx: usize);
    fn set_expression(&mut self, idx: usize);
}
```

- 内置实现：`PixelCat`（配色换装 ×4、表情钉选 ×4）
- **新形象 = 再写一个 trait 实现**（帧序列模型已内置），主程序状态机、菜单、交互零改动

### 3.3 状态机

```
Idle ──散步──> Walk ──撞墙(40%)──> Climb（贴墙爬升→顶部悬挂 1.2s→下行→落地）
 │⑳              │                       拖拽甩出>0.4px/ms
 │长按/拖         ▼                            ▼
 ├──> Sleep（定时器全停，0 CPU）          Thrown（重力+反弹物理）
 ├──> Patted/Shocked（分区触摸反应，定时回落）
 └──全屏前台 → 全部窗口隐藏、动画暂停、瞬态状态落地
```

## 四、功能清单

### 互动
| 功能 | 说明 |
|---|---|
| 目光跟随鼠标 | 每 tick 读一次全局鼠标位置，量化 3×3 方向移动瞳孔 |
| 分区点击 | 头=摸头（笑）、肚子=戳（惊）、脚=踩脚，各配表情+气泡 |
| 长按拖动 | 260ms 长按或拖动超 6px 进入；惊吓脸+蹬腿两帧动画 |
| 甩飞 | 拖拽释放速度 >0.4px/ms 触发：重力抛物线+反弹衰减+落地晕眩 |
| 爬屏幕边缘 | 散步撞墙概率触发，精灵旋转 90°，顶上悬挂后下行 |
| 双击睡觉 | 睡觉态定时器完全停止（真 0 CPU），点击唤醒 |
| 气泡对话框 | 独立点击穿透小窗；互动反馈+发呆随机碎碎念 |

### AI 与语音
| 功能 | 说明 |
|---|---|
| AI 聊天 | 聊天窗（内置 Unifont 中文渲染 + 系统输入法），默认**免费模型** nemotron-3-super:free |
| 图片识别 | 拖 png/jpg/webp/gif 进聊天窗 → vision 请求（默认 deepseek-flash，实测可用） |
| TTS 语音 | LiteGate MiMo 合成（实测可用、限时免费），AI 回复/喂食/提醒可发声，右键开关 |
| 离线降级 | 未配置 api_key 时功能不受影响，聊天给出接入指引 |

### 实用工具
| 功能 | 说明 |
|---|---|
| 喝水/久坐提醒 | 默认 45/90 分钟，气泡+表情+语音，间隔可配置、可开关 |
| 待办清单 | 独立窗：输入新增（中文 IME）、勾选、删除，todos.json 持久化 |
| 扔个窗口 | 右键触发：枚举候选窗口→抛物线动画（SetWindowPos 不抢焦点；跳过最大化/全屏/cloaked 窗口） |

### 联动与多屏
| 功能 | 说明 |
|---|---|
| 键盘联动 | 全局键盘 LL 钩子（回调 100ms 节流、快进快出），打字时猫拍爪 |
| 手柄联动 | XInput 10Hz 轮询，A/B/X/Y 按键触发反应 |
| 多屏适配 | 散步/爬墙/弹跳边界跟随猫所在显示器；跨屏拖动自由；气泡/子窗位置跟随 |

### 外观
| 功能 | 说明 |
|---|---|
| 换装 | 橘猫/白猫/黑猫/粉猫配色方案，右键一键切换 |
| 换表情 | 自动/开心/惊讶/困困（钉选覆盖） |
| PetModel 接口 | 形象扩展点，已含像素猫与帧序列模型两种实现（见 §3.2） |

## 五、关键技术决策

| 决策 | 理由 |
|---|---|
| 软件渲染，不用 GPU | 游戏渲染零竞争；64×64 blit 成本可忽略；无 overlay/注入，反作弊友好 |
| 事件驱动而非轮询 | 所有定时器"有事才存在"：睡觉/全屏时事件循环零唤醒（真 0 CPU） |
| 纯 Rust 而非 Tauri | WebView2 常驻内存不可接受；纯 Rust exe 独立、可交叉编译 |
| 默认免费聊天模型 | 高频操作零成本；vision 单独指到廉价付费模型（免费模型实测不支持图片输入） |
| 内嵌 Unifont 字体 | 16px 点阵风格契合像素猫审美，全 CJK 覆盖，中文气泡/聊天无需系统字体 |
| ureq + rustls（跳过自签校验） | 局域网 LiteGate 网关为自签证书；阻塞式 HTTP 放独立线程，UI 不卡 |
| 密钥只从 toml/环境变量读 | 代码与示例零凭据；`.gitignore` 挡 deskpet.toml |

## 六、质量保障

### 6.1 自动化测试（12 项，全绿）

- 配置解析：默认回退 / 字段覆盖 / 坏 toml 不崩
- AI 历史：system 开头、16 条上限、图片只留最近 3 条
- 精灵：状态→帧映射、4 皮肤 × 9 帧渲染非空、爬墙旋转正确性
- 字体：OTF 解析 + 中英文混排真实出字形
- 多屏：MonRect 中心点归属判定

### 6.2 完整 Review 记录（v0.3.0 前一轮）

**P0（均修复）**
1. ⭐ **windows-gnu + fat LTO 静默裁剪活代码**：链接器 gc-sections 把聊天/待办/字体等全部裁掉，产物变"空壳"且零编译报错（此前每版 exe 均中招）。修复：禁用 LTO + `scripts/verify_build.py` 产物指纹自检（8 个功能签名）
2. 待办点击行索引错位（倒序绘制 × 正序命中）→ 勾选/删除错条目
3. TTS 运行时开关失效（检查了 cfg.voice 而非菜单 voice_on）
4. 全屏遮挡期间提醒气泡/语音仍会弹出 → 现隐藏时静默顺延
5. 甩飞/爬墙中进全屏 → 动画冻结 → 现进入全屏立即落地

**P1（均修复）**：扔窗口改 SetWindowPos 不抢焦点 + 跳过 DWM cloaked 窗口；键盘钩子 100ms 节流；聊天历史图片 base64 只留最近 3 条；TTS 1.5s 节流；子窗定位 clamp 防 panic；编译警告清零。

### 6.3 已实测的接口（对 LiteGate 真实调用）

聊天（免费模型回复正常）✓ · 图片识别（deepseek-flash 正确描述图片）✓ · TTS（合法 24kHz WAV）✓

## 七、性能与资源占用

| 指标 | 数值/机制 |
|---|---|
| exe 体积 | 10.1MB（其中 Unifont 中文库 4.9MB） |
| 内存 | ~10MB（无 WebView/无 GPU 进程） |
| CPU 待机 | ≈0%（眨眼 600ms/帧、碎碎念定时器按需） |
| CPU 散步/打字 | <1% 单核瞬时（180ms/帧软渲染 64×64） |
| CPU 睡觉/全屏 | 0%（事件循环 Wait，零唤醒零重绘） |
| GPU | 0%（无任何图形 API 上下文） |
| 后台线程 | 全屏检测 2s/次（µs 级）；钩子回调快进快出；XInput 10Hz；TTS 按需 |

## 八、安全设计

- API Key 只存在于 `deskpet.toml`（gitignore）或环境变量 `DESKPET_API_KEY`，代码/示例零凭据
- 请求带 `X-LiteGate-App: deskpet` 便于网关按应用核算用量
- 自签证书跳过校验仅针对局域网自托管网关场景
- 无遥测、无外联（除用户配置的网关）、无自启动、无注入/全局钩子外的系统侵入（钩子可配置关闭）

## 九、已知限制与风险

| 项 | 说明 | 缓解/升级路径 |
|---|---|---|
| 命中区域为整窗矩形 | 透明角落挡鼠标 | 升级：Win32 UpdateLayeredWindow 逐像素穿透 |
| ~~Live2D 运行时~~ | **已定案不做**：违背轻量初衷（WebView/Cubism 均重） | 帧序列模型 + Live2D Viewer 预渲染导出帧 |
| 全局键盘钩子 | 个别反作弊环境可能限制 | toml `keyboard_link = false` 关闭 |
| 猛甩时偶发错帧可能 | 手动拖拽低频重绘 | 可回退 OS 模态拖拽 |
| 未实机回归 | 本机为 Linux | 附实机验收清单（附录 D） |

## 十、后续路线图

1. Windows 实机验收（按附录 D 清单）
2. 字体子集化（10.1MB → 约 6MB，代价：生僻字空白）
3. 素材模型加载器（PNG 帧序列 + model.toml 描述，落地"换模型"）
4. ~~Live2D 后端~~（已定案不做；如未来需要，按 PetModel trait 实现）
5. 养成数值（好感度/饱食度）、番茄钟、UpdateLayeredWindow 逐像素穿透
6. 开机自启（可选开关）、多显示器缝隙处理

## 变更记录

### v0.3.2（2026-09-15）
- **右键菜单 v2**：自绘半透明圆角黑底白字菜单，悬停/按压两级高亮，二级页面（互动/换装/表情/设置），开关类条目点击后菜单保持打开并即时刷新；移除 muda 上下文菜单（托盘菜单仍为原生）
- **新动作**：端坐（尾巴摆动）、伸懒腰、舔毛洗脸、低头吃饭（喂食触发）；散步结束随机进入休息姿势，小概率趴到别的窗口顶边上栖息（跟随窗口移动，约 7 秒后跳下）
- **新表情**：星星眼、脸红（表情总数 6）
- **思考反馈**：等待 AI 回复时头顶"·/··/···"跳动文字泡
- **设置页**：键盘联动/手柄联动/目光跟随/跟随鼠标/碎碎念/喝水提醒/久坐提醒/语音播报 全部菜单内可视化开关，实时生效并持久化回 deskpet.toml
- **跟随鼠标模式**：开启后猫主动朝光标水平位置走动，靠近后坐下凝视
- 方案定案（v0.3.3）：预设动作 + 交互表情 + 帧序列换装；Live2D 明确不做，PetModel trait 保留为通用形象扩展点

### v0.3.1（2026-09-15）

### v0.3.1（2026-09-15）
- **交互重构**：移除大聊天窗，改为猫脚下悬浮输入框 + 回复以猫头顶文字泡呈现；上下文保留在内存，可连续对话；拖图识别改为拖进输入框触发
- **字体更换**：Unifont → **Fusion Pixel 12px proportional zh_hans**（缝合像素字体，SIL OFL 1.1，可商用；来源于 TakWolf/fusion-pixel-font release 2025.12.25，许可全文见 assets/OFL-fusion-pixel.txt）
- 修复：rustls 双加密后端（ring + aws-lc-rs 并存）导致启动 panic（Windows 上表现为双击无反应）；新增回归测试
- exe 10.2MB；单元测试 13 项

## 附录 A：构建与发布

```bash
# Linux 交叉编译（依赖 rustup target x86_64-pc-windows-gnu + mingw-w64）
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    cargo build --release --target x86_64-pc-windows-gnu
python3 scripts/verify_build.py   # ⚠️ 必跑：防止链接期静默裁剪复发
# 产物：target/x86_64-pc-windows-gnu/release/deskpet.exe
```

> ⚠️ 本项目在 windows-gnu 目标下**禁止开启 `lto = true`**：会静默裁剪活代码且无任何报错（详见 §6.2 P0-1）。

精灵帧预览：`cargo run --bin dump_frames`（输出 /tmp/pet_frames/*.ppm）
单元测试：`cargo test`（12 项）

## 附录 B：目录结构

```
deskpet-rs/
├── deskpet.exe               # 交付产物（v0.3.0，10.1MB）
├── deskpet.toml.example      # 配置模板
├── assets/unifont.otf        # 内置中文字库（4.9MB）
├── scripts/verify_build.py   # 构建产物自检
├── docs/PROJECT_REPORT.md    # 本报告
└── src/
    ├── main.rs        1448 行  # 事件循环/状态机/多屏/提醒/菜单/扔窗口
    ├── model.rs        125 行  # ★ PetModel 抽象层 + 内置像素猫
    ├── sprites.rs      377 行  # 像素猫绘制（配色/表情参数化）
    ├── text.rs         188 行  # fontdue + Unifont 中文渲染
    ├── bubble.rs       130 行  # 气泡窗（点击穿透）
    ├── chat.rs         335 行  # 聊天窗（IME/拖图识别/历史）
    ├── todo.rs         311 行  # 待办窗（增删勾选/持久化）
    ├── ai.rs           234 行  # OpenAI 兼容客户端 + TTS 合成
    ├── input.rs         82 行  # 键盘 LL 钩子 + XInput 轮询
    ├── tts.rs           58 行  # WAV 播放（PlaySound）
    ├── config.rs       151 行  # 配置读取（默认免费模型）
    └── bin/dump_frames.rs      # 精灵帧调试导出
```

## 附录 C：配置说明（deskpet.toml）

| 字段 | 默认 | 说明 |
|---|---|---|
| base_url |  `https://your-gateway.example/v1` | OpenAI 兼容入口（自建网关，如 LiteGate） |
| api_key | 空 | 网关虚拟密钥；空 = 离线模式；或环境变量 `DESKPET_API_KEY` |
| model | `nvidia/nemotron-3-super-120b-a12b:free` | 聊天模型（免费） |
| vision_model | `deepseek-flash` | 拖图识别模型（免费模型不支持图片输入） |
| pet_name / system_prompt | 团子 / 小猫人设 | 聊天窗标题与 AI 性格 |
| voice / tts_voice | true / mimo_default | TTS 开关与音色（9 种可选） |
| drink_minutes / sit_minutes | 45 / 90 | 提醒间隔（0=关闭） |
| keyboard_link / gamepad_link | true / true | 输入联动开关 |

## 附录 D：Windows 实机验收清单

- [ ] 启动：右下角出现橙色像素猫，透明背景、置顶、无任务栏图标
- [ ] 目光：移动鼠标，猫眼跟随
- [ ] 点击：摸头（笑+泡）/ 戳肚子（惊）/ 踩脚（叫）
- [ ] 拖动：按住 0.3s 起拖，蹬腿动画；快速甩出→抛物线弹跳落地
- [ ] 双击睡觉（Zzz），点击唤醒
- [ ] 散步撞左右边缘触发爬墙（猫贴边竖立）
- [ ] 右键菜单全项：对话/待办/喂食/扔窗口/说句话/换装/表情/提醒/语音/睡觉/关于/退出
- [ ] 对话：中文输入、回车发送、免费模型回复、历史滚动
- [ ] 拖图进聊天窗 → 图片描述回复
- [ ] 语音开 → 回复/提醒出声
- [ ] 全屏视频/游戏 → 猫和所有窗口消失、无声；退出恢复
- [ ] 待办：添加/勾选/删除，重启后 todos.json 还在
- [ ] 双显示器：拖到副屏后散步边界跟随副屏
- [ ] 托盘：对话/退出

---

*报告生成：2026-09-14 · 对应构建 v0.3.0（verify_build 自检通过）*
