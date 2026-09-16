#!/usr/bin/env python3
"""构建产物自检：确认 exe 完整包含全部功能代码（防止链接期静默裁剪复发）。
用法：python3 scripts/verify_build.py [exe 路径]"""
import sys

path = sys.argv[1] if len(sys.argv) > 1 else 'target/x86_64-pc-windows-gnu/release/deskpet.exe'
exe = open(path, 'rb').read()

# 每个功能模块的标志性字符串（源码字面量 → 必须出现在 exe 中）
checks = {
    '内置字体(unifont)': b'OTTO',
    '聊天窗': '思考中'.encode(),
    '待办窗': '待办清单'.encode(),
    '爬墙': '爬墙咯'.encode(),
    '扔窗口': '没有可以扔的窗口喵'.encode(),
    '手柄联动': '手柄真好玩'.encode(),
    '喂食语音': '小鱼干最棒了喵'.encode(),
    'AI 离线提示': '就能聊天啦'.encode(),
    '勿扰模式': '勿扰模式开启'.encode(),
    'TTS 试听': '这是我的声音喵'.encode(),
    '首次引导': '长按拖动、双击睡觉'.encode(),
    'AI 重试': '重试 2 次仍失败'.encode(),
}
missing = [name for name, sig in checks.items() if sig not in exe]
print(f'{path}: {len(exe)} 字节')
for name, sig in checks.items():
    print(f'  {"OK " if sig in exe else "缺失"} {name}')
if missing:
    print(f'构建不完整，缺少: {missing}')
    print('提示：windows-gnu 下不要开启 lto=true（会静默裁剪活代码）')
    sys.exit(1)
print('构建完整 ✓')
