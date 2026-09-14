![截屏2026-03-20 14.17.22](https://github.com/0pen1/PhoneControl/blob/main/docs/img/%E6%88%AA%E5%B1%8F2026-03-20%2014.17.22.png)

# Phone Control

多设备 Android 群控桌面应用，支持实时截屏预览、批量操作、scrcpy 投屏。基于 **Tauri 2 + React + TypeScript** 构建。

## 功能

- **多 ADB 服务器管理** — 支持本地和远程 ADB 服务器，配置持久化，可启用/禁用服务器
- **实时截屏预览** — 当前页设备自动截屏刷新，FPS 可调（1-30）
- **批量控制** — 点击、滑动、文本输入、按键事件广播到所有选中设备，自动坐标缩放
- **单设备直接交互** — 未选中的设备卡片也可直接点击/滑动操控，不影响群控组
- **全屏预览模式** — 一键切换，所有设备按比例缩小显示在一页内，无需翻页滚动
- **设备管理** — 左侧列表和右侧卡片均可禁用/启用设备，禁用设备不显示在投屏列表
- **设备 ID 复制** — 点击设备 ID 快速复制到剪贴板
- **scrcpy 集成** — 一键启动 scrcpy 投屏，支持远程 ADB 服务器
- **ADB Shell** — 对选中设备执行 shell 命令，逐设备显示输出
- **分页显示** — 每页设备数可选（6/8/10/12/14/16/20/24），默认 14 台，自动管理预览
- **设备信息展示** — 自动获取设备型号和电池电量
- **设备筛选** — 按设备 ID 实时过滤
- **自动刷新** — 应用启动和切换服务器状态时自动刷新设备列表
- **深色主题** — 现代化深色 UI

## 技术栈

| 层级 | 技术 |
|------|------|
| 后端 | Rust, Tauri 2.x, Tokio |
| 前端 | React 19, TypeScript, Zustand, Vite |
| 通信 | Tauri Commands + Events |
| 样式 | CSS Modules + Custom Properties |

## 项目结构

```
phone-control/
├── src-tauri/
│   └── src/
│       ├── lib.rs               # Tauri 命令、应用入口
│       ├── state.rs             # AppState
│       ├── config.rs            # 配置持久化
│       └── adb/
│           ├── device.rs        # 设备结构、ADB 输出解析
│           ├── server.rs        # ADB 服务器轮询
│           ├── commands.rs      # tap/swipe/text/keyevent + 坐标缩放
│           └── screenshot.rs    # 异步截屏循环（JPEG 压缩）
├── src/
│   ├── App.tsx
│   ├── store/index.ts           # Zustand 状态管理
│   ├── hooks/                   # useDevices, useScreenshot, useAdbCommands
│   ├── components/
│   │   ├── Sidebar/             # 服务器列表、FPS 滑块、设备列表
│   │   ├── DeviceGrid/          # 设备网格（分页 + 全屏预览）、设备卡片
│   │   └── Toolbar/             # 文本/Shell 模式、按键按钮
│   └── types/index.ts
├── package.json
└── vite.config.ts
```

## 环境要求

- [Node.js](https://nodejs.org/) >= 18
- [Rust](https://www.rust-lang.org/tools/install) >= 1.70

> adb 与 scrcpy 已内置在仓库根目录的 `scrcpy/` 文件夹中，并随安装包一起分发，**无需单独安装**。如需使用系统安装的版本，可设置 `ADB_PATH` / `SCRCPY_PATH` / `SCRCPY_SERVER_PATH` 环境变量覆盖。

## 快速开始

```bash
# 安装依赖
cd phone-control
npm install

# 开发模式
npm run tauri dev

# 构建生产版本
npm run tauri build
```

构建产物：
- **macOS**: `src-tauri/target/release/bundle/macos/phone-control.app`
- **DMG**: `src-tauri/target/release/bundle/dmg/phone-control_0.1.0_x64.dmg`

## 使用说明

### 添加 ADB 服务器
1. 在左侧 "ADB Servers" 区域输入服务器地址和端口（默认 `127.0.0.1:5037`）
2. 点击 "+" 添加服务器
3. 点击服务器状态点可以启用/禁用服务器
4. 切换服务器状态时自动刷新设备列表

### 设备管理
- **左侧设备列表**：
  - 应用启动时自动刷新设备列表
  - 点击设备选择/取消选择
  - 点击状态点禁用/启用设备（禁用后不显示在右侧投屏列表）
  - 点击 ▶ 按钮启动 scrcpy 独立投屏
  - 使用搜索框按设备 ID 筛选
  - 点击 ↻ 按钮手动刷新

- **右侧设备卡片**：
  - 点击卡片头部选择设备并开始投屏
  - 点击投屏画面进行点击和滑动操作
  - 点击设备 ID 复制到剪贴板
  - 点击 ✕ 按钮禁用设备
  - 点击 ▶ 按钮启动 scrcpy 独立投屏
  - 底部 Per page 调整每页显示数量（默认 14 台）
  - 点击底部 ⊞ 按钮进入全屏预览模式，所有设备自动缩放显示在一页内

### 批量操作
1. 使用 "All" 按钮选择所有在线设备
2. 使用 "None" 按钮取消所有选择
3. 在底部工具栏发送文本、按键或 Shell 命令到所有选中设备

## 配置文件

服务器配置保存在 `~/.phone_control/servers.json`：

```json
{
  "servers": [
    { "host": "127.0.0.1", "port": 5037, "enabled": true }
  ]
}
```

## 架构说明

- 截屏在 Rust 端解码 PNG → 缩小至 360px → 重编码 JPEG（~30-60KB），防止 WebView 内存溢出
- 坐标映射为两级转换：前端处理 `object-fit: contain` 偏移计算，后端按设备真实分辨率等比缩放
- 设备轮询完全在 Rust 后台任务运行，通过 `wm size`/`getprop`/`dumpsys battery` 获取真实设备信息，前端通过 Tauri 事件被动接收
- 全屏预览模式通过 `ResizeObserver` 监听容器尺寸变化，动态计算最优列数和缩放比例

## License

MIT
