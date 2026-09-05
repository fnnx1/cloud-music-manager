# cloud-music-manager

网易云音乐歌单管理（当前为简单主流程演示，无 GUI）。

## 当前主流程（`src/main.rs`）

```
拉取歌单（链接/ID） → 筛选歌手包含「洛天依」 → 二维码登录 → 以登录用户为所有者创建新歌单并加入
```

示例输出（歌单 `2191808452`，407 首中筛出 57 首洛天依歌曲）：

```text
[1/5] 正在拉取歌单: https://music.163.com/#/playlist?id=2191808452
歌单「FMxi-喜欢的音乐」共 407 首（其中已下架 0 首）
[2/5] 歌手包含「洛天依」的歌曲：57 首
    1. 乱心/洛天依Official - 涟漪微微动
    …
[3/5] 需要登录后才能创建新歌单
请用网易云音乐 App 扫码登录（90 秒内有效）：
  https://music.163.com/login?codekey=…
```

第 4、5 步（建单、加歌）需要先登录成功。登录由工具**自行管理一套本地独立
Cookie 存储**（`data/cookies.txt`，与浏览器无关）：登录成功即落盘，之后再次
运行**自动复用、免登录**。首次登录按菜单选择方式：

1. **手机号 + 短信验证码（推荐）**：输入手机号 → 工具调网易云发送短信 → 输入
   验证码完成登录，全程无需浏览器、无需复制任何东西；
2. **网页版登录 Cookie**：已在浏览器登录网页版时粘贴 `MUSIC_U`（F12 →
   Application/存储 → Cookies），或设置环境变量 `MUSIC_U=…`（脚本场景）；
3. **手机 App 扫码**：打印二维码 URL，用 App 扫码授权。

> 写操作需要的 `__csrf` Cookie 由工具自动访问 `music.163.com/m/` 补齐。

## 架构：不自己实现 API

- **本项目不含任何自己实现的网易云 API 对接代码**。网络请求、weapi/eapi 加密、
  登录、歌单读写全部直接使用 [`ncm-api-rs`](https://github.com/SPlayer-Dev/ncm-api-rs)
  crate（SPlayer-Dev，NeteaseCloudMusicApi Enhanced 的 Rust 移植，WTFPL）。
- 早期自研的加密 / HTTP 客户端 / DTO 层已全部删除（git 历史可查）。

本仓库只保留产品逻辑：

| 文件 | 内容 |
|---|---|
| `src/lib.rs` | 导出 `model` + `filter`（GUI 可复用） |
| `src/model.rs` | 归一化领域模型 `Song` / `PlaylistInfo`（从 crate 返回的 JSON 直接构建） |
| `src/filter.rs` | `SongFilter` 筛选规则 + `dedupe_by_id` |
| `src/main.rs` | 上述主流程：应用层编排（Cookie 持久化、二维码登录轮询、分页抓取） |

> ⚠️ `ncm-api-rs` 源自 NeteaseCloudMusicApi **Enhanced**，后者衍生自已因版权下架的
> `Binaryify/NeteaseCloudMusicApi`。请仅用于个人学习与管理自己的歌单，尊重版权。

## 使用

```bash
cargo run --release                                # 使用内置示例歌单
cargo run --release -- "<歌单链接或ID>"              # 指定歌单
```

二维码最终确认需手机 App 扫码；登录 Cookie 存于 `data/cookies.txt`，之后可复用
（同一账号再次运行会跳过扫码）。

## 用到的 crate 接口

| 用途 | 方法 |
|---|---|
| 歌单详情（含全部 trackIds） | `ApiClient::playlist_detail`（eapi） |
| 批量歌曲详情（500/批，id 须为数字） | `ApiClient::song_detail`（weapi） |
| 二维码登录 | `login_qr_key` / `login_qr_create` / `login_qr_check`（800 过期 / 801 等扫码 / 802 已扫 / 803 成功） |
| 账号信息 | `user_account` |
| 创建歌单 / 加歌 | `playlist_create` / `playlist_tracks`（**manipulate/tracks，op=add**；不要用 `playlist_track_add`，网页会话下会 401「无权限操作歌单」） |

登录态 = `MUSIC_U` Cookie（写操作还需 `__csrf`，程序会自动访问 `music.163.com/m/`
补齐并持久化到 `data/cookies.txt`）。

## 风控提示

网易对机房/海外出口 IP 有风控（登录类接口可能返回 `-462`），个人住宅网络正常。
遇到时可给请求带 `X-Real-IP`（国内 IP）或走代理。

## 路线图（建议后续）

1. 筛选引擎化（`SongFilter` 扩展：多条件 AND/OR、年代/专辑分组等）；
2. 本地歌单缓存管理（浏览/合并/对比）；
3. GUI（egui/iced）：复用 `model.rs` / `filter.rs`，网络层直接依赖 crate。
