# cloud-music-manager

网易云音乐歌单管理：粘贴歌单链接 → 拉取并存本地副本 → 按条件组合筛选
（生成副本、不改原歌单）→ 预览当前列表 → 登录后以你的账号创建新歌单。

默认启动 **egui 图形界面**；旧命令行流程保留（`--cli`）。

## 运行

```bash
cargo run                        # 图形界面
cargo run -- --cli "<歌单链接>"   # 命令行：拉取→手动输入关键词筛选→登录→建单加歌
```

## 命令行（`--cli`）

拉取歌单后，会提示手动输入**筛选关键词**（匹配 歌名/歌手/专辑，回车留空 = 全部保留，
并自动剔除已下架），随后预览结果 → 登录 → 以你的账号创建 `[原歌单名]-<关键词>精选`
并加入歌曲。

```bash
cargo run -- --cli                # 使用默认歌单（云音乐热歌榜）
cargo run -- --cli "<歌单链接或ID>"
```

## 图形界面（`src/gui.rs`）

| 区域 | 功能 |
|---|---|
| 歌单输入 | 粘贴链接/ID + 拉取（回车也可）；成功后把原始歌单存为本地副本 `data/playlist-<id>.json` |
| 左侧信息卡 | 封面图（自动下载）+ 歌单名/创建者/曲数/播放数 |
| 组合筛选 | 关键词 / 歌手包含 / 专辑包含 / 时长区间 / 发行年份区间 / 仅VIP / 排除VIP / 剔除下架 / 按ID去重 |
| 「完成」按钮 | 把筛选结果**生成一份副本**（原歌单表不被改动），展示在中间“当前状态列表” |
| 当前状态列表 | 表格展示：序号/歌名/歌手/专辑/时长/年份/状态（下架、VIP） |
| 「创建歌单」 | 歌单名可改，默认 `[原歌单名]-[筛选条件]`；未登录会弹登录窗口 |
| 登录窗口 | 手机号+短信验证码，或“使用本地已保存的 Cookie”（工具独立存储于 `data/cookies.txt`） |

后台网络操作在独立线程串行执行，界面不阻塞；登录态 Cookie 由程序自行保存/复用。

## 架构：不自己实现 API

- 所有网易云请求/加密/登录/歌单读写直接使用 [`ncm-api-rs`](https://github.com/SPlayer-Dev/ncm-api-rs)
  crate（SPlayer-Dev，NeteaseCloudMusicApi Enhanced 的 Rust 移植，WTFPL）。
- 本项目只保留产品逻辑：`src/model.rs`（归一化模型）、`src/filter.rs`（筛选）、
  `src/gui.rs`（界面）、`src/main.rs`（入口 + CLI）。
- ⚠️ 仅供个人学习与管理自己的歌单，尊重版权。

## 文件

```text
src/
  main.rs   入口：默认 GUI，`--cli` 走命令行；CLI 逻辑在 main.rs 内 cli_main
  gui.rs    egui 界面（后台 worker + 通道通信）
  lib.rs    model + filter（GUI/CLI 共用）
  model.rs  归一化 Song/PlaylistInfo/PlaylistDump
  filter.rs 筛选规则
data/       运行时数据（本地副本、Cookie），已被 gitignore
```
