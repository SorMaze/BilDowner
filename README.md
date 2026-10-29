# BilDowner

[![Release](https://github.com/SorMaze/BilDowner/actions/workflows/release.yml/badge.svg)](https://github.com/SorMaze/BilDowner/actions/workflows/release.yml)
[![License: BSD-2-Clause](https://img.shields.io/badge/License-BSD--2--Clause-blue.svg)](LICENSE)

BilDowner 是一个纯 Rust 命令行 Bilibili / 抖音 / X 视频下载器，不包含前端页面。Bilibili 通过 `x/player/wbi/playurl` 获取 DASH 流，分别下载视频和音频，默认由内置 MP4 封装器逐样本合并 AVC/H.264 与 AAC 轨道；也可以显式选择 FFmpeg。两种方式都不进行有损重新编码。抖音通过分享页公开的 SSR 作品数据解析 MP4 播放流，优先下载无水印地址，并在不可用时自动回退到原始播放地址。X 下载器从官方 syndication JSON 解析 `video.twimg.com` MP4 变体，默认下载最高码率版本。
当前版本：**1.2.0**。

请只下载你有权保存的内容。本工具不会绕过 DRM、付费或账号权限；接口返回的清晰度取决于当前账号本身具有的播放权限。

## 功能

- 终端二维码登录，或手工导入 Cookie
- Windows 下使用 DPAPI 按当前 Windows 用户加密保存 Cookie
- 查看和选择分 P；支持 BV/AV 视频以及 `ep`/`ss` 番剧、影视链接
- 支持 `360p`、`480p`、`720p`、`1080p`、`1080p+`、`1080p60`、`4k`、`hdr`、`dolby`、`8k` 和原始 `qn`
- 可选择 AVC/H.264、HEVC/H.265 或 AV1；默认优先兼容性较好的 AVC
- `merged`、`separate`、`both` 三种输出模式
- 视频与音频并行下载，支持 `.part` 文件续传和 DASH 备用 CDN
- 支持无音轨视频；接口返回 `audio: null` 时保存纯视频，不添加静音音轨
- Bilibili 支持 `-t/--time` 下载选中分 P 的指定时间段；按分段索引请求局部数据，纯 Rust 无损封装
- 内置纯 Rust MP4 合并器，默认 AVC/H.264 + AAC 下载无需安装 FFmpeg；可通过 `--ffmpeg` 显式切换
- 支持抖音短分享链接和 `douyin.com/video/<作品 ID>` 链接，无需登录或额外插件
- 支持 `x.com` / `twitter.com` 帖子视频，默认最高码率，也可用 `--quality` 限制分辨率

## 环境

- 支持 edition 2024 的当前稳定版 Rust
- 默认不需要 FFmpeg；内置合并当前支持 AVC/H.264 + AAC
- 完整下载时，HEVC/H.265、AV1 可使用 `--mode separate` 保留原始流，或通过 `--ffmpeg` 合并；`-t` 时间段下载当前支持 AVC/H.264 + AAC

## 使用

```powershell
# 编译
cargo build --release

# 查看应用帮助；--help 显示完整说明，-h 显示简要提示
cargo run -- --help
cargo run -- download --help

# 推荐：扫码登录
cargo run -- auth qr

# 检查登录状态
cargo run -- auth status

# 也可以从标准输入导入浏览器 Cookie，避免出现在命令历史中
Get-Clipboard | cargo run -- auth set

# 查看分 P、清晰度和编码
cargo run -- info "https://www.bilibili.com/video/BV1xx411c7mD"
cargo run -- info BV1xx411c7mD --page 1
cargo run -- info BV1xx411c7mD --page "1-3,5,7"
cargo run -- info "https://www.bilibili.com/bangumi/play/ss43164" --page 2

# 默认下载最高可用清晰度，同时保留分离流和合并 MP4
cargo run -- download BV1xx411c7mD

# 选择清晰度、分 P、编码和输出模式
cargo run -- download BV1xx411c7mD --page 2 --quality 1080p --codec avc --mode both
cargo run -- download BV1xx411c7mD --page "1-7" --quality 1080p
cargo run -- download BV1xx411c7mD --page "5,7" --quality 1080p
cargo run -- download BV1xx411c7mD --quality 4k --codec hevc --mode merged --ffmpeg
cargo run -- download BV1xx411c7mD --quality 720p --mode separate --output-dir .\downloads
cargo run -- download ep693247 --quality 1080p --mode both

# 查看并下载抖音分享视频（抖音为已合并的单个 MP4）
cargo run -- info "https://v.douyin.com/KyuVvC8wEu4/"
cargo run -- download "https://v.douyin.com/KyuVvC8wEu4/"

# 查看并下载 X 帖子中的主视频（公开帖子无需登录）
cargo run -- info "https://x.com/Kimi_Moonshot/status/2077521842080817296"
cargo run -- download "https://x.com/Kimi_Moonshot/status/2077521842080817296"
cargo run -- download "https://x.com/Kimi_Moonshot/status/2077521842080817296" --quality 1080p
```

`--page` 支持单个序号（`7`）、闭区间（`1-7`）、逗号列表（`5,7`）及其组合。显式 `--page` 优先于 Bilibili 链接中的 `p=`；未指定时会使用链接中的分 P，链接也未指定时默认 P1。对没有分 P 的视频传入 `--page` 不会中止，而是忽略参数并显示警告。

下载 Release 中的 EXE 后，可直接使用 `bildowner.exe` 替换上面命令中的 `cargo run --`，例如 `bildowner.exe download --help`。

清晰度必须通过 `-q/--quality` 指定，不能直接写在链接后面：`-q 112` 与 `-q 1080p+` 等价，`-q 120` 与 `-q 4k` 等价。省略时使用 `best`，即当前账号实际可用的最高档位。

### 下载时间段（1.2.0）

用 `-t`（或 `--time`）指定 `[起点-终点]`；省略 `-t` 时完整下载。

| 格式 | 示例 | 请求区间 |
| --- | --- | --- |
| `[HH:MM:SS-HH:MM:SS]` | `"[00:00:07-00:00:12]"` | 第 7～12 秒 |
| `[MM:SS-MM:SS]` | `"[00:07-00:12]"` | 第 7～12 秒 |
| `[秒-秒]` | `"[7-12]"` | 第 7～12 秒 |

方括号可省略，秒数可带最多三位小数，例如 `"7.5-12.250"`。建议始终用引号包裹区间，避免 shell 对方括号或空格作额外解释。`MM:SS` 的分钟可以大于 59，冒号后的秒以及 `HH:MM:SS` 的分钟须小于 60。

下列命令都请求 P2 的第 7～12 秒：

```powershell
cargo run -- download BV19s4y1G7td -p 2 -q 112 -t "[00:00:07-00:00:12]"
cargo run -- download BV19s4y1G7td -p 2 -q 112 -t "[00:07-00:12]"
cargo run -- download BV19s4y1G7td -p 2 -q 112 -t "[7-12]"

# 同一区间应用到每个选中的分 P
cargo run -- download BV19s4y1G7td -p "2,5" -q 112 -t "[7-12]" --mode merged

# 秒数支持小数，也可使用长参数名
cargo run -- download BV19s4y1G7td --page 2 --quality 112 --time "7.5-12.250"
```

时间从各分 P 开头单独计起。必须满足 `0 <= 起点 < 终点`；起点超出时长时拒绝下载，终点超过时长则截到实际结尾。无损模式的起点对齐到前一个可独立解码的关键帧分段，终端会显示实际区间，例如 `[7-12]` 可能实际保存 `[5-12]`。终点按媒体帧边界处理，通过 MP4 编辑列表限制播放区间，保留解码 B 帧所需的参考数据，不重新编码。

下载器先读取 DASH 初始化数据和 SIDX 索引，再使用 HTTP Range 请求覆盖目标区间的媒体分段。分段是最小网络下载单位，边界分段内可能有额外帧；不会先完整下载再裁切。CDN 不支持 Range 或索引不合法时会尝试备用地址，所有地址都失败则报错，不回退到完整下载。

此功能当前支持 Bilibili 普通视频和番剧/影视的 AVC/H.264 视频与标准 AAC 音频，无需 FFmpeg；可以指定 `--codec avc`。不与 `--ffmpeg` 同时使用。抖音、X 和其他编码的时间段下载暂不支持，传入时会明确报错。

时间段文件名带 `-t起点毫秒-终点毫秒ms`，避免与完整文件或其他区间重名。`merged` 输出 `.mp4`；`separate` 输出可直接播放的 `.video.mp4` 和 `.audio.m4a`（无音轨时只输出视频）；`both` 保留这三种文件。时间段下载失败重试时重新请求所选分段；完整下载的 `.part` 续传保持不变。

### 输出模式与文件

输出模式通过 `-m/--mode` 指定，默认 `both`：

| 模式 | 完整下载 | `-t` 时间段下载 |
| --- | --- | --- |
| `merged` | 最终 `.mp4` | 最终 `.mp4` |
| `separate` | `.video.m4s`、`.audio.m4s` | `.video.mp4`、`.audio.m4a` |
| `both` | 分离 M4S 与最终 MP4 | 分离视频 MP4、音频 M4A 与最终 MP4 |

如果该分 P 没有标准音频流，则省略音频文件，最终 MP4 为纯视频。`merged` 在成功后清理临时流文件。完整下载的内置封装器和 FFmpeg 都支持无音轨素材。

重复运行时，已有目标文件默认跳过；使用 `--force` 覆盖。时间段以请求值命名，例如 `[7-12]` 对应 `-t7000-12000ms`，即使起点实际对齐到 5 秒，文件名仍保留请求区间。

完整下载默认使用内置合并器。需要 FFmpeg 时可显式启用，或指定自定义路径；`--ffmpeg` 与 `-t/--time` 不能同时使用：

```powershell
cargo run -- download BV1xx411c7mD --mode merged --ffmpeg
cargo run -- download BV1xx411c7mD --mode merged --ffmpeg C:\Tools\ffmpeg\bin\ffmpeg.exe
```

Cookie 默认保存在 `%APPDATA%\BilDowner\cookie.dat`，Windows 下通过 DPAPI 加密，只有保存它的 Windows 用户可以解密。可用 `BILDOWNER_COOKIE_FILE` 覆盖路径：

```powershell
$env:BILDOWNER_COOKIE_FILE = 'D:\private\bildowner-cookie.dat'
cargo run -- auth status
cargo run -- auth clear
```

## 实现说明

1. 使用 `x/web-interface/view` 解析 BV/AV 号，使用 `pgc/view/web/season` 解析 `ep`/`ss` 剧集、标题和 `cid`。
2. 从 `x/web-interface/nav` 获取当前 WBI 图片密钥，从官方前端脚本动态提取混淆表，生成 `wts` 和 `w_rid` 签名。
3. 普通视频使用 WBI playurl，番剧/影视使用 PGC playurl，并通过 `qn=<清晰度>&fnval=4048&fourk=1` 请求 DASH；如果接口标记为 DRM 流则明确拒绝下载。
4. 从同一清晰度的 `dash.video` 中按编码选择视频流，从 `dash.audio` 选择最高码率标准音频流。
5. 下载时携带 Bilibili Referer、浏览器 User-Agent 和已保存 Cookie；主 CDN 失败时依次尝试备用地址。
6. 默认合并时用纯 Rust 解析 fragmented MP4，复制 H.264/AAC 压缩样本、时间戳与关键帧信息，并重建普通 MP4 的 `mdat` 和样本索引；传入 `--ffmpeg` 时改用 FFmpeg 的 `copy` 模式封装。
7. 指定 `-t` 时，根据 SIDX 索引选择可独立解码的起始分段，以 HTTP Range 请求所需字节区间，检查 `206` 和 `Content-Range`，并在封装时重建时间轴、用编辑列表限制播放终点。完整下载流程保持独立。
8. 抖音链接先解析为作品 ID，再读取 `www.douyin.com/share/video/<作品 ID>` 页面内的 `_ROUTER_DATA`；下载器优先尝试 `play` 无水印地址，并以页面提供的 `playwm` 地址兜底。
9. X 链接提取帖子 ID 后请求官方 syndication JSON，读取首个视频的 MP4 变体；默认按码率选择最高档，指定 `--quality` 时选择不高于目标高度的最佳档位。Windows 使用系统 WinINet 网络路径，其他平台使用 Rustls。

站点接口可能变化；遇到问题时先运行 `cargo run -- info <链接>`。Bilibili 会给出当前账号实际可用的清晰度和编码，抖音会显示分享页公开的作品信息，X 会列出帖子首个视频的全部 MP4 档位。

## 开源许可证

本项目以 [BSD 2-Clause License](LICENSE) 开源，Copyright (c) 2026 SorMaze。
