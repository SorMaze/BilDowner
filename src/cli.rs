use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const PAGE_HELP: &str = "Bilibili 分 P/剧集序号：单个序号、闭区间或逗号列表，例如 7、1-7、1-3,5,7。\n显式 --page 优先于链接中的 p=；均未指定时使用 P1。单分 P 视频忽略此选项。";

const DOWNLOAD_EXAMPLES: &str = "示例：
  bildowner download BV19s4y1G7td -p 2 -q 112
  bildowner download BV19s4y1G7td -p 2 -q 112 -t \"[00:00:07-00:00:12]\"
  bildowner download BV19s4y1G7td -p 2 -q 112 -t \"[00:07-00:12]\"
  bildowner download BV19s4y1G7td -p \"2,5\" -q 112 -t \"[7-12]\" --mode merged

-t 使用各分 P 自己的时间轴；无损起点可能提前到关键帧，终端会显示实际区间。
时间段下载支持 Bilibili AVC/H.264 + AAC，无需 FFmpeg；完整下载省略 -t。";

#[derive(Debug, Parser)]
#[command(
    name = "bildowner",
    version,
    about = "Bilibili / 抖音 / X 视频下载器",
    after_help = "快速开始：\n  bildowner auth qr\n  bildowner info BV19s4y1G7td\n  bildowner download BV19s4y1G7td -p 2 -t \"[7-12]\"\n\n查看下载选项与时间格式：bildowner download --help"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 登录、保存或清除 Bilibili Cookie
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// 查看视频信息；Bilibili 还会显示分 P、清晰度和编码
    Info {
        /// BV/AV/ep/ss 号、Bilibili/抖音链接或 X 帖子链接
        input: String,
        /// 分 P：单个序号、闭区间或逗号列表，例如 7、1-7、5,7
        #[arg(short, long, value_name = "PAGES", long_help = PAGE_HELP)]
        page: Option<String>,
    },
    /// 下载完整视频，或用 -t 下载 Bilibili 指定时间段
    #[command(after_help = DOWNLOAD_EXAMPLES)]
    Download(DownloadArgs),
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// 在终端显示二维码，扫码登录并加密保存 Cookie
    Qr,
    /// 手工保存 Cookie；不传参数时从标准输入读取
    Set {
        /// Cookie 字符串，例如 SESSDATA=...; bili_jct=...
        #[arg(long, conflicts_with = "cookie_file")]
        cookie: Option<String>,
        /// 从文件读取 Cookie
        #[arg(long, value_name = "PATH", conflicts_with = "cookie")]
        cookie_file: Option<PathBuf>,
    },
    /// 检查本地 Cookie 和当前登录状态
    Status,
    /// 删除本地保存的 Cookie
    Clear,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// BV/AV/ep/ss 号、Bilibili/抖音链接或 X 帖子链接
    pub input: String,

    /// 清晰度：best、360p、480p、720p、1080p、1080p+、1080p60、4k、hdr、dolby、8k 或数值 qn
    #[arg(
        short,
        long,
        default_value = "best",
        long_help = "清晰度：best、360p、480p、720p、1080p、1080p+、1080p60、4k、hdr、dolby、8k，或数值 qn。\n例如 1080p+ 或 112、4k 或 120；Bilibili 的 best 选择当前账号可用的最高档位。\n必须通过 -q/--quality 指定，例如 -q 112，不能作为链接后的额外位置参数。"
    )]
    pub quality: String,

    /// 分 P：单个序号、闭区间或逗号列表，例如 7、1-7、5,7
    #[arg(short, long, value_name = "PAGES", long_help = PAGE_HELP)]
    pub page: Option<String>,

    /// 下载 Bilibili 时间段；纯 Rust 无损，起点对齐到前一个关键帧
    #[arg(
        short = 't',
        long,
        value_name = "RANGE",
        conflicts_with = "ffmpeg",
        long_help = "下载时间段，支持以下格式（建议用引号包裹）：\n  [HH:MM:SS-HH:MM:SS]，例如 \"[00:00:07-00:00:12]\"\n  [MM:SS-MM:SS]，例如 \"[00:07-00:12]\"\n  [秒-秒]，例如 \"[7-12]\"\n\n方括号可省略，秒数最多支持三位小数，例如 \"7.5-12.250\"。\n同一区间应用到 --page 选中的每个分 P，时间从各分 P 开头单独计起。\n必须满足 0 <= 起点 < 终点；起点超出时长报错，终点超出时长则截到实际结尾。\n\n通过分段索引和 HTTP Range 只请求所需分段；无损起点可能提前，终端会显示实际区间。\n当前支持 Bilibili AVC/H.264 视频与标准 AAC 音频（也支持无音轨视频）；可指定 --codec avc。\n不支持抖音、X、HEVC/AV1 时间段下载，且不能与 --ffmpeg 同时使用。"
    )]
    pub time: Option<crate::time_range::TimeRange>,

    /// 视频编码偏好
    #[arg(
        long, value_enum, default_value_t = CodecChoice::Auto,
        long_help = "视频编码偏好；auto 优先 AVC/H.264，其次 HEVC/H.265、AV1。\n内置合并与 -t 时间段下载支持 AVC/H.264；完整下载的其他编码可用 --mode separate，或通过 --ffmpeg 合并。"
    )]
    pub codec: CodecChoice,

    /// 输出模式；both 会同时保留分离流和合并文件
    #[arg(
        short, long, value_enum, default_value_t = DownloadMode::Both,
        long_help = "输出模式（Bilibili）：\n  merged：只保留最终 .mp4。\n  separate：完整下载输出 .video.m4s 和 .audio.m4s；-t 输出 .video.mp4 和 .audio.m4a。\n  both：同时保留分离流与最终 .mp4（默认）。\n无音轨时省略音频文件。时间段文件名包含请求区间，避免与完整视频或其他区间重名。"
    )]
    pub mode: DownloadMode,

    /// 输出目录
    #[arg(short, long, default_value = "downloads")]
    pub output_dir: PathBuf,

    /// 改用 FFmpeg 合并；可省略 PATH 以使用 PATH 中的 ffmpeg
    #[arg(
        long, value_name = "PATH", num_args = 0..=1, default_missing_value = "ffmpeg",
        long_help = "完整下载时改用 FFmpeg 无重新编码合并；省略 PATH 时使用 PATH 中的 ffmpeg。\n默认由内置封装器合并 AVC/H.264 + AAC，无需安装 FFmpeg。此选项不能与 -t/--time 同时使用。"
    )]
    pub ffmpeg: Option<PathBuf>,

    /// 覆盖已经存在的文件
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CodecChoice {
    /// 优先 AVC/H.264，其次 HEVC、AV1
    Auto,
    Avc,
    Hevc,
    Av1,
}

impl CodecChoice {
    pub fn codecid(self) -> Option<u32> {
        match self {
            Self::Auto => None,
            Self::Avc => Some(7),
            Self::Hevc => Some(12),
            Self::Av1 => Some(13),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DownloadMode {
    /// 只输出最终 MP4；源视频有音轨时保留音频
    Merged,
    /// 只输出分离流；完整下载为 M4S，时间段下载为视频 MP4 和音频 M4A
    Separate,
    /// 同时输出分离流和合并 MP4
    Both,
}

impl DownloadMode {
    pub fn needs_merge(self) -> bool {
        matches!(self, Self::Merged | Self::Both)
    }

    pub fn keeps_separate(self) -> bool {
        matches!(self, Self::Separate | Self::Both)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::{Cli, Command};

    #[test]
    fn parses_time_ranges_with_page_selection_and_rejects_invalid_input() {
        for value in ["[00:00:07-00:00:12]", "[00:07-00:12]", "[7-12]"] {
            let cli = Cli::try_parse_from([
                "bildowner",
                "download",
                "BV19s4y1G7td",
                "-p",
                "2,5",
                "-t",
                value,
            ])
            .unwrap();
            let Command::Download(args) = cli.command else {
                panic!("expected download");
            };
            assert_eq!(args.page.as_deref(), Some("2,5"));
            assert_eq!(args.time.unwrap().start_ms, 7000);
            assert_eq!(args.time.unwrap().end_ms, 12000);
        }
        assert!(
            Cli::try_parse_from(["bildowner", "download", "BV19s4y1G7td", "-t", "[12-7]"]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "bildowner",
                "download",
                "BV19s4y1G7td",
                "-t",
                "[7-12]",
                "--ffmpeg"
            ])
            .is_err()
        );
    }

    #[test]
    fn ffmpeg_is_opt_in_with_optional_path() {
        let cli = Cli::try_parse_from(["bildowner", "download", "BV1xx411c7mD"]).unwrap();
        let Command::Download(args) = cli.command else {
            panic!("expected download command");
        };
        assert!(args.ffmpeg.is_none());
        assert!(args.page.is_none());

        let cli =
            Cli::try_parse_from(["bildowner", "download", "BV1xx411c7mD", "--ffmpeg"]).unwrap();
        let Command::Download(args) = cli.command else {
            panic!("expected download command");
        };
        assert_eq!(args.ffmpeg.as_deref(), Some(Path::new("ffmpeg")));

        let cli = Cli::try_parse_from([
            "bildowner",
            "download",
            "BV1xx411c7mD",
            "--ffmpeg",
            "C:/Tools/ffmpeg.exe",
        ])
        .unwrap();
        let Command::Download(args) = cli.command else {
            panic!("expected download command");
        };
        assert_eq!(
            args.ffmpeg.as_deref(),
            Some(Path::new("C:/Tools/ffmpeg.exe"))
        );
    }

    #[test]
    fn keeps_page_selection_expression() {
        let cli =
            Cli::try_parse_from(["bildowner", "download", "BV1xx411c7mD", "--page", "1-3,5,7"])
                .unwrap();
        let Command::Download(args) = cli.command else {
            panic!("expected download command");
        };
        assert_eq!(args.page.as_deref(), Some("1-3,5,7"));

        let cli =
            Cli::try_parse_from(["bildowner", "info", "BV1xx411c7mD", "--page", "7"]).unwrap();
        let Command::Info { page, .. } = cli.command else {
            panic!("expected info command");
        };
        assert_eq!(page.as_deref(), Some("7"));
    }
}
