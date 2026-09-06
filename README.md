# cloud-music-manager（网易云歌单管理器）

本项目是纯 rust 实现的网易云音乐歌单管理：可以按条件筛选/排序给定链接/ID的歌单，并可在登录后上传处理后的歌单到自己的账号，由此实现歌单快速编辑。

默认启动 **egui 图形界面**。

## 运行

```bash
cargo run                        # 图形界面（用我！）
cargo run -- --cli "<歌单链接>"   # 命令行（仅供演示，功能较少）：拉取→手动输入关键词筛选→登录→建单加歌
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

## 架构

- 所有网易云请求/加密/登录/歌单读写直接使用 [`ncm-api-rs`](https://github.com/SPlayer-Dev/ncm-api-rs)；
  crate（SPlayer-Dev，NeteaseCloudMusicApi Enhanced 的 Rust 移植，WTFPL）。
- 本项目只保留新增逻辑：`src/model.rs`（归一化模型）、`src/filter.rs`（筛选）、
  `src/gui.rs`（界面）、`src/main.rs`（入口 + CLI）；
- ⚠️ cookie 内容目前以明文存储到 `data/cookies.txt`，请勿分享或上传该文件。

## 致谢

- [`ncm-api-rs`](https://github.com/SPlayer-Dev/ncm-api-rs)（SPlayer-Dev）——NeteaseCloudMusicApi
  Enhanced 的 Rust 移植（WTFPL），本项目所有网络请求 / 加密 / 登录能力均来自它；
- NeteaseCloudMusicApi / Enhanced 社区项目——接口协议的事实来源；
- [`egui` / `eframe`](https://github.com/emilk/egui)（Emil Ernerfeldt 及贡献者）——即时模式 GUI 框架；
- [`tokio`](https://github.com/tokio-rs/tokio)、[`reqwest`](https://github.com/seanmonstar/reqwest)、
  [`image`](https://github.com/image-rs/image)、[`serde`](https://github.com/serde-rs/serde) 等 Rust 生态库。


## 免责声明

> 本项目自身代码中不含任何对网易云接口协议进行逆向的代码；所有网络/加密/登录相关实现均由第三方 crate `ncm-api-rs` 提供，并按该 crate 自身的 WTFPL 许可分发。
> 本项目不提供任何音乐下载、破解 VIP 类功能；
> 请尊重歌曲版权与网易云音乐服务条款，因使用本工具产生的一切后果由使用者自行承担。

> 以上为作者的非约束性提示，不构成对 WTFPL 许可授权范围的任何限制。



## 许可

本项目采用 **WTFPL** 发布，见 [`LICENSE`](./LICENSE)。

## 文件

```text
src/
  main.rs   入口：默认 GUI，`--cli` 走命令行；CLI 逻辑在 main.rs 内 cli_main
  gui.rs    egui 界面（后台 worker + 通道通信）
  lib.rs    model + filter（GUI/CLI 共用）
  model.rs  归一化 Song/PlaylistInfo/PlaylistDump
  filter.rs 筛选规则
data/       运行时数据（本地副本、Cookie）
```
