# cloud-music-manager

网易云音乐歌单管理工具（第一阶段：API 对接，无 GUI）。

## 目标工作流

1. 用户粘贴一个**歌单链接**；
2. 软件调用网易云 API 抓取歌单全部歌曲并**本地归一化整理**（JSON 缓存）；
3. 用户用「类似本地播放器」的逻辑**按条件筛选**（时长、VIP、关键词、去重、年代…，见 `src/ncm/filter.rs`）；
4. 整理好之后调用网易云 API **创建新歌单并批量加入歌曲**（此步需要登录，采用**二维码扫码登录**）。

当前以命令行子命令打通整条链路，为后续 GUI 做验证。

## 技术选型（调研结论）

**API 层**直接使用现成的 Rust 实现 [`SPlayer-Dev/ncm-api-rs`](https://github.com/SPlayer-Dev/ncm-api-rs)
（crates.io 名 `ncm-api-rs`，WTFPL）。它完整实现了 weapi / eapi / linuxapi 加密，
封装了登录（手机号 / 邮箱 / 二维码）、歌单读写等 300+ 接口，是
NeteaseCloudMusicApi Enhanced 的 Rust 移植。

> ⚠️ 使用说明：该 crate 源自 `NeteaseCloudMusicApi Enhanced`，后者衍生自
> **已因版权问题下架**的 `Binaryify/NeteaseCloudMusicApi`。请仅用于个人学习与
> 管理自己的歌单，尊重版权，勿用于下载/分发无版权内容或商业用途。

**本仓库的增量价值**在 crate 之上，未重复造轮子：
- 强类型的领域模型 `Song` / `PlaylistInfo`（`src/ncm/types.rs`），把原始 JSON
  归一化为后续筛选/管理可直接使用的结构（含"是否已下架"标记）；
- 本地「播放器式」筛选引擎雏形 `SongFilter`（`src/ncm/filter.rs`）；
- 歌单链接 / ID / 短链解析、登录态 Cookie 本地持久化（`data/cookies.txt`）；
- CLI 演示：`fetch`（抓取+筛选+缓存）→ `push`（登录+建新歌单+批量加歌）闭环。

没有发现开箱即用、完整覆盖「链接导入 → 本地筛选 → 重建新歌单」闭环的现成软件，
这正是本项目自研的部分。

## 目录结构

```text
src/
  lib.rs          库入口（GUI 可复用）
  main.rs         CLI 演示（fetch / login / status / push）
  ncm/
    api.rs        NcmClient：封装 ncm-api-rs + 链接解析 + Cookie 持久化
    types.rs      归一化领域模型 + 原始 DTO
    filter.rs     本地筛选规则（未来筛选引擎雏形）
    error.rs      错误类型
examples/
  quick_fetch.rs  最小用法示例（GUI/工具参考）
```

## 使用

```bash
cargo build --release

# 抓取热歌榜并去重、丢弃 VIP 歌曲，保存到 data/playlist-3778678.json
./target/release/cloud-music-manager fetch "https://music.163.com/#/playlist?id=3778678" --dedupe --drop-vip

# 二维码登录（Cookie 存到 data/cookies.txt，之后可复用）
./target/release/cloud-music-manager login

# 查看登录状态
./target/release/cloud-music-manager status

# 登录后：创建新歌单并推送整理结果（也支持直接读本地缓存文件）
./target/release/cloud-music-manager push data/playlist-3778678.json --name "我的精选" --privacy

# 最小库用法示例
cargo run --example quick_fetch -- 3778678
```

筛选选项：`--dedupe`（按歌手+歌名去重）、`--drop-vip`（去 VIP）、`--drop-unavailable`
（去已下架）、`--min-seconds N`（最短时长）、`--keyword 词`（歌名/歌手/专辑）。

## 相关 API 端点（由 ncm-api-rs 内部实现）

| 用途 | 加密 | 路径（参数） |
|---|---|---|
| 歌单元数据 + 全部 trackIds | eapi | `/api/v6/playlist/detail`（id, s=8） |
| 批量歌曲详情（500/批） | weapi | `/api/v3/song/detail`（ids 逗号分隔，**数字**） |
| 二维码 key | eapi | `/api/login/qrcode/unikey` |
| 二维码 URL | - | `https://music.163.com/login?codekey=<unikey>` |
| 二维码轮询 | eapi | `/api/login/qrcode/client/login`（800 过期 / 801 等扫码 / 802 待确认 / 803 成功） |
| 账号信息 | weapi | `/api/nuser/account/get` |
| 创建歌单 | weapi | `/api/playlist/create`（name, privacy: 0/10） |
| 添加歌曲 | weapi | `/api/playlist/track/add`（300/批；返回 502=已在歌单） |
| 删除歌曲 | weapi | `/api/playlist/track/delete` |

登录态 = `MUSIC_U` Cookie（自动捕获 `Set-Cookie`，可持久化到本地文件）。

## 风控提示

- 网易对**机房 / 海外出口 IP** 有风控（登录类接口返回 `-462 检测到您的网络环境
  存在风险`）。个人住宅网络一般不会遇到。
- 若在云服务器/海外部署，可设置 `NCM_REAL_IP=<国内住宅IP>` 环境变量（会发送
  `X-Real-IP` 请求头），必要时配代理。
- 二维码轮询状态码：`800` 过期、`801` 等待扫码、`802` 已扫待确认、`803` 成功。

## 路线图（建议后续）

1. **筛选引擎**：把 `SongFilter` 扩展成规则组合（歌手/年代/专辑/时长区间、
   多关键词 AND/OR、保留策略等）；
2. **本地管理**：歌单缓存浏览/对比/批量操作（复制、删减、合并多个歌单）；
3. **GUI**：基于 `egui`/`iced` 呈现「本地播放器式」列表 + 筛选面板；
4. **写回增强**：更新新歌单简介/标签、删除原歌单等（crate 接口已具备）。
