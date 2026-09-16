# deskpet-rs

纯 Rust 实现的 Windows 桌面宠物（像素猫），主打**极低资源占用**：
单文件 exe 约 9.7MB（内含 4.6MB Fusion Pixel 像素中文字库），运行内存 ~10MB，
**不创建任何 GPU/D3D/OpenGL 上下文**，对游戏、视频渲染零竞争。

## 功能总览

**互动**
- 目光跟随鼠标（3×3 方向量化，只在动画 tick 读一次鼠标）
- 分区点击：摸头 / 戳肚子 / 踩脚，各有表情 + 气泡
- 长按拖动（惊吓脸 + 蹬腿动画）；**快速甩出 → 物理抛物线弹跳落地**
- 双击睡觉 / 点击唤醒
- **爬屏幕边缘**：散步撞墙概率爬墙，顶上挂一会儿再下来（精灵自动旋转 90°）
- 气泡对话框（点击穿透）+ 发呆随机碎碎念；**气泡贴身跟随**，宠物走动时气泡跟着挪
- **聊天窗打开时安静陪打字**：不自顾自散步/入睡，也不追鼠标；右键菜单打开期间暂停位移
- AI 请求串行：上一条没回复完再发会得到"等我说完"提示，文本不丢；失败自动重试（最多 3 次）
- **勿扰模式**：右键一键开启——停碎碎念、停提醒、AI 回复不出声，互动照常
聊天记录可落盘（chat_log.jsonl，设置页可关，隐私敏感请自行权衡）；↑/↓ 翻阅已发送消息
- **全局快捷键**：Ctrl+Shift+D 勿扰 / T 待办 / C 聊天 / H 隐藏显示 / Q 退出（设置页可关）
- **首次运行引导**：第一次启动会提示基本玩法（只出现一次）
- **关于**：右键→关于，显示版本、当前内存占用与项目地址

**高 DPI**：菜单/输入框/气泡全按显示器缩放渲染（125%/150% 缩放笔记本正常显示）

**AI 与语音**
- 聊天窗：内置 Unifont 点阵中文字体、系统输入法、回车发送、历史记录
- 接 OpenAI 兼容接口（默认 LiteGate 网关，聊天默认免费模型，拖图识别走视觉模型）
- **思考反馈**：等待 AI 回复时头顶冒"···"跳动的省略号文字泡
- **TTS 语音**：LiteGate MiMo 合成（限时免费），AI 回复/喂食/提醒可发声，右键可开关
- **图片识别**：拖 png/jpg/webp/gif 进聊天窗

**实用工具**
- **喝水/久坐提醒**：气泡 + 语音，间隔可配置，右键可关
- **待办清单**：独立小窗，勾选/删除/新增；`!高`/`!低` 设优先级（色条可点击循环切换）、
  `@17:30` 设截止（显示"剩 X 时"，过期标红）；按优先级排序；到期时宠物气泡提醒一次；
  todos.json 持久化（旧格式自动兼容）
- **扔个窗口**：右键触发，把桌面上最大的窗口抛物线扔出去（不碰最大化和全屏窗口）

**联动与多屏**
- **键盘联动**：全局键盘钩子（回调只发事件），打字时猫跟着拍爪
- **手柄联动**：XInput 10Hz 轮询，按 A/B/X/Y 猫有反应
- **多屏适配**：散步/爬墙/弹跳边界跟随猫所在的显示器，跨屏拖动自由

**外观扩展（PetModel 抽象层）**
- **多模型**：右键▸模型，内置像素猫 / 柴犬 / 兔兔 / 企鹅 四个物种（PetModel 工厂切换）
- **方案定案**：预设动作 + 交互表情 + 帧序列换装，不引入 Live2D 运行时（需要新形象时用帧序列模型：Live2D Viewer 预渲染导出透明帧即可接入）
- **帧序列模型（自制模型接入）**：把 PNG 动画帧按 `models/<模型名>/` 规范放置即可新增模型——表情/动作/换装都是不同的帧序列，直接播放；Live2D 动作可在 Live2D Viewer 里预渲染导出成透明帧后接入（无需内嵌 Live2D 运行时）。模型列表按名称稳定排序，重启后选中的模型不会漂移
- 换装：每个物种独立的配色方案（菜单二级页选择，带当前标记）
- 换表情：自动 / 开心 / 惊讶 / 困困 / 星星眼 / 脸红（二级页钉选）
- 猫模型 v2：自动描边、口鼻/胡须/内耳细节；新增动作——端坐（尾巴摆）、伸懒腰、舔毛（洗脸）、吃饭（低头对碗）
- 设置页：键盘联动/手柄联动/目光跟随/跟随鼠标/碎碎念/喝水提醒/久坐提醒/语音 全部可视化开关，写回 deskpet.toml
- **`PetModel` trait 是给未来模型预留的接口**：`render(Pose) → 像素缓冲` +
  `costumes()/set_expression()/set_costume()`。将来接 Live2D 或自制素材模型，
  只需实现该 trait，主程序、菜单、交互全部复用

## 使用

1. 把 `deskpet.exe` 拷到 Windows 10/11 x64 机器，双击运行
2. AI/语音功能：复制 `deskpet.toml.example` 为 `deskpet.toml` 放在 exe 旁边，
   填 `api_key`（LiteGate 管理台签发；也可用环境变量 `DESKPET_API_KEY`）
3. 右键猫 = 主菜单（对话/待办/互动/外观/提醒/语音/睡觉/关于/退出）

## 构建（Linux 交叉编译）

```bash
rustup target add x86_64-pc-windows-gnu
# 需要 mingw-w64：apt install gcc-mingw-w64-x86-64-win32
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    cargo build --release --target x86_64-pc-windows-gnu
# 产物：target/x86_64-pc-windows-gnu/release/deskpet.exe
```

预览精灵帧：`cargo run --bin dump_frames`（输出 /tmp/pet_frames/*.ppm）

**构建后务必自检**（防止链接期静默裁剪复发）：

```bash
python3 scripts/verify_build.py target/x86_64-pc-windows-gnu/release/deskpet.exe
```

## 代码结构

```
src/
├── main.rs      # 事件循环、状态机（散步/爬墙/甩飞/坐卧/趴窗）、多屏、提醒、菜单
├── menu.rs      # 自绘右键菜单（半透明圆角/二级页/悬停按压态）
├── model.rs     # ★ PetModel 抽象层（换模型/换装/表情接口）+ 内置像素猫
├── sprites.rs   # 像素猫绘制（配色/表情参数化）
├── text.rs      # fontdue + Unifont 中文渲染
├── bubble.rs    # 气泡窗（点击穿透）
├── chat.rs      # 聊天窗（IME + 拖图识别）
├── todo.rs      # 待办窗（勾选/删除/持久化）
├── ai.rs        # OpenAI 兼容客户端 + MiMo TTS
├── input.rs     # 键盘 LL 钩子 + XInput 轮询
├── tts.rs       # TTS 播放（PlaySound SND_ASYNC）
└── config.rs    # deskpet.toml（密钥只从配置/环境变量来）
```

## 轻量设计纪律

- 事件驱动：每个定时器只在有事情发生时存在，睡觉/全屏时事件循环零唤醒
- 钩子回调只投递事件；手柄 10Hz 轮询；TTS/扔窗口在独立线程
- 全屏检测每 2 秒一次微秒级调用，前台全屏时隐藏全部窗口暂停一切动画

## 自制帧序列模型

```text
models/示例猫/
├── model.toml              # name / fps / expressions / [[costume]]
└── default/                # 一套换装 = 一个子目录
    ├── idle_0..3.png       # 片段 = 同前缀 PNG 序列（64x64 RGBA）
    ├── walk_0.png walk_1.png ...
    ├── happy_0.png  shock_0.png  dragged_0..1.png
    ├── sit_0..1.png  stretch_0.png  groom_0..1.png
    ├── eat_0..1.png  climb_0..1.png  perch_0..1.png
    └── expr_1_0.png ...    # 表情片段 expr_<表情下标>_<帧>
```

- 缺失片段自动回退 idle；`cargo run --bin export_example_model` 可导出一份示例
- 换装 = 换一整套不同帧序列；表情 = expr 片段循环播放
- Live2D 用户路线：在 Live2D Viewer 导出动作/表情的透明帧序列 → 按此结构放置（不内嵌运行时）

## 已知限制与重要说明

- **windows-gnu 目标禁止开启 `lto = true`**：fat LTO 会与链接器 --gc-sections
  相互作用，把活代码（聊天/待办/字体等）静默裁剪掉，产物变成"空壳"且无任何
  编译报错。已默认关闭，并用 scripts/verify_build.py 在构建后自检。

- 猫窗口是整块矩形命中区域（透明角落挡鼠标）；升级路径 Win32 UpdateLayeredWindow
- **已定案不内嵌 Live2D 运行时**（Webview/Cubism 均违背轻量初衷）；PetModel 接口保留，未来如有需求可另写实现
- 键盘钩子为全局低级钩子，个别反作弊环境可能限制（可 config 关闭）
- 未在真实 Windows 实机回归，IME/钩子/TTS/多屏需要实机验证
