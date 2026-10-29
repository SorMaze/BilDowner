use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, COOKIE, ORIGIN, REFERER};
use serde::Deserialize;
use tokio::sync::OnceCell;
use url::Url;

use crate::cli::CodecChoice;

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36 Edg/138.0.0.0";

const VIEW_API: &str = "https://api.bilibili.com/x/web-interface/view";
const NAV_API: &str = "https://api.bilibili.com/x/web-interface/nav";
const PLAY_API: &str = "https://api.bilibili.com/x/player/wbi/playurl";
const SEASON_API: &str = "https://api.bilibili.com/pgc/view/web/season";
const PGC_PLAY_API: &str = "https://api.bilibili.com/pgc/player/web/playurl";
const VIDEO_PAGE: &str = "https://www.bilibili.com/video/";
const MAX_FRONTEND_SCRIPTS: usize = 16;
const MAX_FRONTEND_SCRIPT_SIZE: usize = 8 * 1024 * 1024;

pub struct BiliClient {
    client: reqwest::Client,
    cookie: Option<String>,
    mixin_key_enc_tab: OnceCell<[usize; 64]>,
}

impl BiliClient {
    pub fn new(cookie: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("无法创建 HTTP 客户端")?;
        Ok(Self {
            client,
            cookie,
            mixin_key_enc_tab: OnceCell::new(),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .get(url)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
            .header(ORIGIN, "https://www.bilibili.com")
            .header(REFERER, "https://www.bilibili.com/");
        if let Some(cookie) = &self.cookie {
            request.header(COOKIE, cookie)
        } else {
            request
        }
    }

    pub fn media_get(&self, url: &str, referer: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .get(url)
            .header(REFERER, referer)
            .header(ORIGIN, "https://www.bilibili.com")
            .header(ACCEPT, "*/*");
        if let Some(cookie) = &self.cookie {
            request.header(COOKIE, cookie)
        } else {
            request
        }
    }

    fn frontend_get(&self, url: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .get(url)
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/javascript,text/javascript;q=0.9,*/*;q=0.8",
            )
            .header(ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
            .header(REFERER, "https://www.bilibili.com/");
        if let Some(cookie) = &self.cookie {
            request.header(COOKIE, cookie)
        } else {
            request
        }
    }

    pub async fn video_info(&self, input: &str) -> Result<VideoInfo> {
        let id = parse_video_id(input)?;
        match id {
            VideoId::Bvid(value) => self.normal_video_info("bvid", &value).await,
            VideoId::Aid(value) => self.normal_video_info("aid", &value).await,
            VideoId::Episode(ep_id) => self.season_video_info(Some(ep_id), None).await,
            VideoId::Season(season_id) => self.season_video_info(None, Some(season_id)).await,
        }
    }

    async fn normal_video_info(&self, key: &str, value: &str) -> Result<VideoInfo> {
        self.get(VIEW_API)
            .query(&[(key, value)])
            .send()
            .await
            .context("请求视频信息失败")?
            .error_for_status()
            .context("视频信息接口返回 HTTP 错误")?
            .json::<ApiEnvelope<VideoInfo>>()
            .await
            .context("解析视频信息失败")?
            .into_data("获取视频信息")
    }

    async fn season_video_info(
        &self,
        ep_id: Option<u64>,
        season_id: Option<u64>,
    ) -> Result<VideoInfo> {
        let request = if let Some(ep_id) = ep_id {
            self.get(SEASON_API).query(&[("ep_id", ep_id)])
        } else {
            self.get(SEASON_API)
                .query(&[("season_id", season_id.context("缺少 season_id")?)])
        };
        let response = request
            .send()
            .await
            .context("请求番剧/影视信息失败")?
            .error_for_status()
            .context("番剧/影视信息接口返回 HTTP 错误")?
            .json::<PgcEnvelope<SeasonInfo>>()
            .await
            .context("解析番剧/影视信息失败")?;
        let season = response.into_result("获取番剧/影视信息")?;
        let episodes = if let Some(ep_id) = ep_id {
            season
                .episodes
                .into_iter()
                .filter(|episode| episode.id == ep_id)
                .collect::<Vec<_>>()
        } else {
            season.episodes
        };
        if episodes.is_empty() {
            bail!("番剧/影视响应中没有可下载的剧集");
        }
        let first = &episodes[0];
        Ok(VideoInfo {
            bvid: first.bvid.clone(),
            aid: first.aid,
            title: season.title,
            pages: episodes
                .into_iter()
                .enumerate()
                .map(|(index, episode)| VideoPage {
                    cid: episode.cid,
                    page: index + 1,
                    part: episode.display_title(),
                    duration: episode.duration / 1000,
                    bvid: Some(episode.bvid),
                    ep_id: Some(episode.id),
                })
                .collect(),
        })
    }

    pub async fn nav(&self) -> Result<NavData> {
        let response = self
            .get(NAV_API)
            .send()
            .await
            .context("请求 Bilibili 导航信息失败")?
            .error_for_status()
            .context("导航接口返回 HTTP 错误")?
            .json::<ApiEnvelope<NavData>>()
            .await
            .context("解析 Bilibili 导航信息失败")?;
        response
            .data
            .context("导航接口没有返回 data（WBI 密钥不可用）")
    }

    pub async fn play_info(&self, bvid: &str, cid: u64, qn: u32) -> Result<PlayData> {
        let nav = self.nav().await?;
        let mixin_key_enc_tab = self.mixin_key_enc_tab(bvid).await?;
        let mixin_key = make_mixin_key(&nav.wbi_img, mixin_key_enc_tab)?;
        let mut params = BTreeMap::from([
            ("bvid".to_owned(), bvid.to_owned()),
            ("cid".to_owned(), cid.to_string()),
            ("fnval".to_owned(), "4048".to_owned()),
            ("fnver".to_owned(), "0".to_owned()),
            ("fourk".to_owned(), "1".to_owned()),
            ("qn".to_owned(), qn.to_string()),
        ]);
        sign_wbi(&mut params, &mixin_key)?;

        self.get(PLAY_API)
            .query(&params)
            .header(REFERER, format!("https://www.bilibili.com/video/{bvid}"))
            .send()
            .await
            .context("请求播放地址失败")?
            .error_for_status()
            .context("播放地址接口返回 HTTP 错误")?
            .json::<ApiEnvelope<PlayData>>()
            .await
            .context("解析播放地址失败")?
            .into_data("获取播放地址")
    }

    async fn mixin_key_enc_tab(&self, bvid: &str) -> Result<&[usize; 64]> {
        self.mixin_key_enc_tab
            .get_or_try_init(|| async { self.fetch_mixin_key_enc_tab(bvid).await })
            .await
    }

    async fn fetch_mixin_key_enc_tab(&self, bvid: &str) -> Result<[usize; 64]> {
        let page_url =
            Url::parse(&format!("{VIDEO_PAGE}{bvid}")).context("无法构造 Bilibili 视频页 URL")?;
        let html = self
            .frontend_get(page_url.as_str())
            .send()
            .await
            .context("请求 Bilibili 视频页失败（无法获取 WBI 混淆表）")?
            .error_for_status()
            .context("Bilibili 视频页返回 HTTP 错误（无法获取 WBI 混淆表）")?
            .text()
            .await
            .context("读取 Bilibili 视频页失败（无法获取 WBI 混淆表）")?;
        let script_urls = frontend_script_urls(&page_url, &html);
        if script_urls.is_empty() {
            bail!("Bilibili 视频页中没有可识别的官方前端脚本，无法获取 WBI 混淆表");
        }

        let mut failures = Vec::new();
        for script_url in script_urls.into_iter().take(MAX_FRONTEND_SCRIPTS) {
            let response = match self
                .frontend_get(script_url.as_str())
                .header(REFERER, page_url.as_str())
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
            {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("{}: {error}", script_url.path()));
                    continue;
                }
            };
            if response
                .content_length()
                .is_some_and(|length| length > MAX_FRONTEND_SCRIPT_SIZE as u64)
            {
                failures.push(format!("{}: 文件过大", script_url.path()));
                continue;
            }
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    failures.push(format!("{}: {error}", script_url.path()));
                    continue;
                }
            };
            if bytes.len() > MAX_FRONTEND_SCRIPT_SIZE {
                failures.push(format!("{}: 文件过大", script_url.path()));
                continue;
            }
            let script = String::from_utf8_lossy(&bytes);
            if let Some(table) = extract_mixin_key_enc_tab(&script) {
                return Ok(table);
            }
        }

        let detail = if failures.is_empty() {
            String::new()
        } else {
            format!("；部分脚本读取失败：{}", failures.join("；"))
        };
        bail!("未能从 Bilibili 当前前端脚本中识别 WBI 混淆表，前端算法可能已改变{detail}")
    }

    pub async fn play_info_for_page(
        &self,
        bvid: &str,
        cid: u64,
        ep_id: Option<u64>,
        qn: u32,
    ) -> Result<PlayData> {
        if let Some(ep_id) = ep_id {
            self.pgc_play_info(ep_id, qn).await
        } else {
            self.play_info(bvid, cid, qn).await
        }
    }

    async fn pgc_play_info(&self, ep_id: u64, qn: u32) -> Result<PlayData> {
        let play = self
            .get(PGC_PLAY_API)
            .query(&[
                ("ep_id", ep_id.to_string()),
                ("qn", qn.to_string()),
                ("fnver", "0".to_owned()),
                ("fnval", "4048".to_owned()),
                ("fourk", "1".to_owned()),
            ])
            .header(
                REFERER,
                format!("https://www.bilibili.com/bangumi/play/ep{ep_id}"),
            )
            .send()
            .await
            .context("请求番剧/影视播放地址失败")?
            .error_for_status()
            .context("番剧/影视播放地址接口返回 HTTP 错误")?
            .json::<PgcEnvelope<PlayData>>()
            .await
            .context("解析番剧/影视播放地址失败")?
            .into_result("获取番剧/影视播放地址")?;
        if play.code.unwrap_or(0) != 0 {
            bail!(
                "获取番剧/影视播放地址失败（{}）：{}",
                play.code.unwrap_or_default(),
                play.message.as_deref().unwrap_or("未知错误")
            );
        }
        if play.is_drm {
            bail!("该剧集返回了 DRM 保护流，本工具不会绕过 DRM");
        }
        Ok(play)
    }
}

pub async fn print_info(client: &BiliClient, input: &str, page: Option<&str>) -> Result<()> {
    let info = client.video_info(input).await?;
    println!("{}\nBV: {}  AV: {}", info.title, info.bvid, info.aid);
    println!("分 P：");
    for item in &info.pages {
        println!("  P{}  {:>6}s  {}", item.page, item.duration, item.part);
    }

    let selection = info.resolve_pages(input, page)?;
    print_page_selection_notice(&info, page, &selection, "检查");
    for page_number in selection.pages {
        let selected = info.page(page_number)?;
        let play = client
            .play_info_for_page(info.page_bvid(selected), selected.cid, selected.ep_id, 127)
            .await?;
        println!("\nP{} 可用清晰度：", selected.page);
        for quality in play.available_qualities() {
            let codecs = play
                .dash
                .as_ref()
                .map(|dash| {
                    dash.video
                        .iter()
                        .filter(|stream| stream.id == quality)
                        .map(|stream| codec_name(stream.codecid, &stream.codecs))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            println!(
                "  {:>3}  {:<14} {}",
                quality,
                play.quality_description(quality),
                codecs
            );
        }
    }
    if client.cookie.is_none() {
        println!("\n当前未登录；登录后通常可获得更高清晰度。");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    pub data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct PgcEnvelope<T> {
    code: i64,
    #[serde(default)]
    message: String,
    result: Option<T>,
}

impl<T> PgcEnvelope<T> {
    fn into_result(self, operation: &str) -> Result<T> {
        if self.code != 0 {
            bail!("{operation}失败（{}）：{}", self.code, self.message);
        }
        self.result
            .with_context(|| format!("{operation}成功，但响应中没有 result"))
    }
}

impl<T> ApiEnvelope<T> {
    pub fn into_data(self, operation: &str) -> Result<T> {
        if self.code != 0 {
            bail!("{operation}失败（{}）：{}", self.code, self.message);
        }
        self.data
            .with_context(|| format!("{operation}成功，但响应中没有 data"))
    }
}

#[derive(Debug, Deserialize)]
pub struct VideoInfo {
    pub bvid: String,
    pub aid: u64,
    pub title: String,
    #[serde(default)]
    pub pages: Vec<VideoPage>,
}

impl VideoInfo {
    pub fn page(&self, page: usize) -> Result<&VideoPage> {
        if page == 0 {
            bail!("分 P 序号必须从 1 开始");
        }
        self.pages
            .get(page - 1)
            .with_context(|| format!("P{page} 不存在；该视频共有 {} 个分 P", self.pages.len()))
    }

    pub fn page_bvid<'a>(&'a self, page: &'a VideoPage) -> &'a str {
        page.bvid.as_deref().unwrap_or(&self.bvid)
    }

    pub fn resolve_pages(&self, input: &str, selection: Option<&str>) -> Result<PageSelection> {
        if self.pages.is_empty() {
            bail!("视频信息中没有可下载的分 P");
        }
        if self.pages.len() == 1 {
            return Ok(PageSelection {
                pages: vec![1],
                source: PageSelectionSource::SinglePage,
            });
        }

        if let Some(selection) = selection {
            return Ok(PageSelection {
                pages: parse_page_selection(selection, self.pages.len())?,
                source: PageSelectionSource::Explicit,
            });
        }
        if let Some(page) = page_from_url(input)? {
            self.page(page)?;
            return Ok(PageSelection {
                pages: vec![page],
                source: PageSelectionSource::Url,
            });
        }
        Ok(PageSelection {
            pages: vec![1],
            source: PageSelectionSource::Default,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSelectionSource {
    Explicit,
    Url,
    Default,
    SinglePage,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PageSelection {
    pub pages: Vec<usize>,
    pub source: PageSelectionSource,
}

pub fn print_page_selection_notice(
    info: &VideoInfo,
    explicit: Option<&str>,
    selection: &PageSelection,
    action: &str,
) {
    match selection.source {
        PageSelectionSource::SinglePage if explicit.is_some() => eprintln!(
            "警告：该视频没有分 P；已忽略 --page `{}`，将{}唯一的视频。",
            explicit.unwrap_or_default(),
            action
        ),
        PageSelectionSource::Default => eprintln!(
            "提示：检测到 {} 个分 P，未指定 --page，默认{} P1。",
            info.pages.len(),
            action
        ),
        PageSelectionSource::Explicit | PageSelectionSource::Url => println!(
            "已选择分 P：{}",
            selection
                .pages
                .iter()
                .map(|page| format!("P{page}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PageSelectionSource::SinglePage => {}
    }
}

#[derive(Debug, Deserialize)]
pub struct VideoPage {
    pub cid: u64,
    pub page: usize,
    pub part: String,
    pub duration: u64,
    #[serde(default)]
    pub bvid: Option<String>,
    #[serde(default)]
    pub ep_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SeasonInfo {
    title: String,
    #[serde(default)]
    episodes: Vec<SeasonEpisode>,
}

#[derive(Debug, Deserialize)]
struct SeasonEpisode {
    id: u64,
    aid: u64,
    bvid: String,
    cid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    long_title: String,
    #[serde(default)]
    duration: u64,
}

impl SeasonEpisode {
    fn display_title(&self) -> String {
        match (self.title.trim(), self.long_title.trim()) {
            ("", "") => format!("ep{}", self.id),
            ("", long) => long.to_owned(),
            (title, "") => title.to_owned(),
            (title, long) => format!("{title} {long}"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct NavData {
    #[serde(rename = "isLogin")]
    pub is_login: bool,
    pub uname: Option<String>,
    pub mid: Option<u64>,
    pub wbi_img: WbiImages,
}

#[derive(Debug, Deserialize)]
pub struct WbiImages {
    img_url: String,
    sub_url: String,
}

#[derive(Debug, Deserialize)]
pub struct PlayData {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    is_drm: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accept_quality: Vec<u32>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub accept_description: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub support_formats: Vec<SupportFormat>,
    pub dash: Option<Dash>,
}

impl PlayData {
    pub fn available_qualities(&self) -> Vec<u32> {
        let mut qualities = self
            .dash
            .as_ref()
            .map(|dash| {
                dash.video
                    .iter()
                    .map(|stream| stream.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| self.accept_quality.clone());
        qualities.sort_unstable_by(|a, b| b.cmp(a));
        qualities.dedup();
        qualities
    }

    pub fn quality_description(&self, id: u32) -> String {
        if let Some(format) = self.support_formats.iter().find(|item| item.quality == id) {
            return format
                .new_description
                .as_deref()
                .or(format.display_desc.as_deref())
                .unwrap_or_else(|| quality_name(id))
                .to_owned();
        }
        if let Some(index) = self
            .accept_quality
            .iter()
            .position(|quality| *quality == id)
            && let Some(description) = self.accept_description.get(index)
        {
            return description.clone();
        }
        quality_name(id).to_owned()
    }

    pub fn select_streams(
        &self,
        requested_quality: &str,
        codec: CodecChoice,
    ) -> Result<(&DashStream, Option<&DashStream>, String)> {
        let dash = self
            .dash
            .as_ref()
            .context("接口没有返回 DASH 音视频流，无法进行音视频分离下载")?;
        let available = self.available_qualities();
        let quality = parse_quality(requested_quality, &available)?;
        let candidates = dash
            .video
            .iter()
            .filter(|stream| stream.id == quality)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            bail!(
                "清晰度 {} 当前不可用；可用值：{}",
                requested_quality,
                available
                    .iter()
                    .map(|id| format!("{}({id})", self.quality_description(*id)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let video = if let Some(codecid) = codec.codecid() {
            candidates
                .iter()
                .copied()
                .find(|stream| stream.codecid == codecid)
                .with_context(|| {
                    let available = candidates
                        .iter()
                        .map(|stream| codec_name(stream.codecid, &stream.codecs))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("该清晰度没有所选编码；可用编码：{available}")
                })?
        } else {
            [7, 12, 13]
                .into_iter()
                .find_map(|codecid| {
                    candidates
                        .iter()
                        .copied()
                        .find(|stream| stream.codecid == codecid)
                })
                .unwrap_or(candidates[0])
        };
        let audio = dash.audio.iter().max_by_key(|stream| stream.bandwidth);
        Ok((video, audio, self.quality_description(quality)))
    }
}

#[derive(Debug, Deserialize)]
pub struct SupportFormat {
    pub quality: u32,
    pub new_description: Option<String>,
    pub display_desc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Dash {
    #[serde(default, deserialize_with = "null_as_default")]
    pub video: Vec<DashStream>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub audio: Vec<DashStream>,
}

// Bilibili uses both null and missing fields for absent lists (e.g. silent videos).
fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub struct DashStream {
    pub id: u32,
    pub base_url: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub backup_url: Vec<String>,
    #[serde(default)]
    pub bandwidth: u64,
    #[serde(default)]
    pub codecs: String,
    #[serde(default)]
    pub codecid: u32,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub frame_rate: String,
    pub segment_base: Option<SegmentBase>,
    #[serde(rename = "SegmentBase")]
    pub legacy_segment_base: Option<SegmentBase>,
}

#[derive(Debug, Deserialize)]
pub struct SegmentBase {
    #[serde(alias = "Initialization")]
    pub initialization: String,
    #[serde(alias = "indexRange")]
    pub index_range: String,
}

impl DashStream {
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.base_url.as_str()).chain(self.backup_url.iter().map(String::as_str))
    }
}

enum VideoId {
    Bvid(String),
    Aid(String),
    Episode(u64),
    Season(u64),
}

fn page_from_url(input: &str) -> Result<Option<usize>> {
    let Ok(url) = Url::parse(input.trim()) else {
        return Ok(None);
    };
    let Some(value) = url
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("p"))
        .map(|(_, value)| value.into_owned())
    else {
        return Ok(None);
    };
    let page = value
        .parse::<usize>()
        .with_context(|| format!("链接中的分 P 参数 `p={value}` 不合法"))?;
    if page == 0 {
        bail!("链接中的分 P 参数必须从 1 开始");
    }
    Ok(Some(page))
}

fn parse_page_selection(value: &str, page_count: usize) -> Result<Vec<usize>> {
    let value = value.trim();
    if value.is_empty() {
        bail!("--page 不能为空");
    }

    let mut pages = Vec::new();
    let mut seen = BTreeSet::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            bail!("--page `{value}` 中存在空的分 P 序号");
        }
        if let Some((start, end)) = item.split_once('-') {
            if end.contains('-') {
                bail!("无法识别分 P 范围 `{item}`");
            }
            let start = parse_page_number(start, page_count)?;
            let end = parse_page_number(end, page_count)?;
            if start > end {
                bail!("分 P 范围 `{item}` 的起点不能大于终点");
            }
            for page in start..=end {
                if seen.insert(page) {
                    pages.push(page);
                }
            }
        } else {
            let page = parse_page_number(item, page_count)?;
            if seen.insert(page) {
                pages.push(page);
            }
        }
    }
    Ok(pages)
}

fn parse_page_number(value: &str, page_count: usize) -> Result<usize> {
    let value = value.trim();
    let page = value
        .parse::<usize>()
        .with_context(|| format!("分 P 序号 `{value}` 不合法"))?;
    if page == 0 {
        bail!("分 P 序号必须从 1 开始");
    }
    if page > page_count {
        bail!("P{page} 不存在；该视频共有 {page_count} 个分 P");
    }
    Ok(page)
}

fn parse_video_id(input: &str) -> Result<VideoId> {
    let re = Regex::new(r"(?i)(BV[0-9A-Za-z]+|av[0-9]+|ep[0-9]+|ss[0-9]+)").expect("static regex");
    let value = re
        .find(input.trim())
        .map(|value| value.as_str())
        .context("无法从输入中找到 BV、AV、ep 或 ss 号")?;
    if value[..2].eq_ignore_ascii_case("bv") {
        Ok(VideoId::Bvid(value.to_owned()))
    } else if value[..2].eq_ignore_ascii_case("av") {
        Ok(VideoId::Aid(value[2..].to_owned()))
    } else if value[..2].eq_ignore_ascii_case("ep") {
        Ok(VideoId::Episode(value[2..].parse().context("ep 号不合法")?))
    } else {
        Ok(VideoId::Season(value[2..].parse().context("ss 号不合法")?))
    }
}

fn frontend_script_urls(page_url: &Url, html: &str) -> Vec<Url> {
    let initial_state_regex =
        Regex::new(r#""insertScripts"\s*:\s*(\[[^\]]*\])"#).expect("static regex");
    let script_tag_regex = Regex::new(r#"(?is)<script\b[^>]*\bsrc\s*=\s*(?:"([^"]+)"|'([^']+)')"#)
        .expect("static regex");
    let mut raw_urls = Vec::new();
    for captures in initial_state_regex.captures_iter(html) {
        if let Ok(urls) = serde_json::from_str::<Vec<String>>(&captures[1]) {
            raw_urls.extend(urls);
        }
    }
    raw_urls.extend(script_tag_regex.captures_iter(html).filter_map(|captures| {
        captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|value| value.as_str().to_owned())
    }));

    let mut seen = HashSet::new();
    raw_urls
        .into_iter()
        .filter_map(|raw_url| page_url.join(&raw_url).ok())
        .filter(is_trusted_frontend_script_url)
        .filter(|url| seen.insert(url.as_str().to_owned()))
        .collect()
}

fn is_trusted_frontend_script_url(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host == "bilibili.com"
            || host.ends_with(".bilibili.com")
            || host == "hdslb.com"
            || host.ends_with(".hdslb.com")
    })
}

fn extract_mixin_key_enc_tab(script: &str) -> Option<[usize; 64]> {
    let array_regex =
        Regex::new(r"\[((?:\s*\d{1,3}\s*,){63}\s*\d{1,3}\s*)\]").expect("static regex");
    let mut best: Option<(u32, [usize; 64])> = None;
    let mut best_is_ambiguous = false;

    for captures in array_regex.captures_iter(script) {
        let values = captures[1]
            .split(',')
            .map(str::trim)
            .map(str::parse::<usize>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?;
        let table: [usize; 64] = values.try_into().ok()?;
        let mut seen = [false; 64];
        if table
            .iter()
            .any(|&value| value >= seen.len() || std::mem::replace(&mut seen[value], true))
        {
            continue;
        }

        let whole_match = captures.get(0).expect("whole regex match");
        let start = whole_match.start().saturating_sub(1024);
        let end = (whole_match.end() + 1024).min(script.len());
        let context = String::from_utf8_lossy(&script.as_bytes()[start..end]);
        let score = [
            ("w_rid", 8),
            ("slice(0,32)", 4),
            ("charAt", 3),
            ("wts", 3),
            ("imgKey", 2),
            ("subKey", 2),
            ("mixin", 2),
        ]
        .into_iter()
        .filter_map(|(needle, points)| context.contains(needle).then_some(points))
        .sum();

        match &best {
            None => {
                best = Some((score, table));
                best_is_ambiguous = false;
            }
            Some((best_score, _)) if score > *best_score => {
                best = Some((score, table));
                best_is_ambiguous = false;
            }
            Some((best_score, best_table)) if score == *best_score && table != *best_table => {
                best_is_ambiguous = true;
            }
            _ => {}
        }
    }

    best.filter(|_| !best_is_ambiguous).map(|(_, table)| table)
}

fn make_mixin_key(images: &WbiImages, mixin_key_enc_tab: &[usize; 64]) -> Result<String> {
    fn file_stem(url: &str) -> Result<String> {
        let url = Url::parse(url).context("WBI 图片 URL 不合法")?;
        let file = url
            .path_segments()
            .and_then(Iterator::last)
            .context("WBI 图片 URL 没有文件名")?;
        Ok(file.split('.').next().unwrap_or(file).to_owned())
    }

    let source = format!(
        "{}{}",
        file_stem(&images.img_url)?,
        file_stem(&images.sub_url)?
    );
    let chars = source.chars().collect::<Vec<_>>();
    if chars.len() < 64 {
        bail!("WBI 密钥长度异常");
    }
    Ok(mixin_key_enc_tab
        .iter()
        .filter_map(|index| chars.get(*index))
        .take(32)
        .collect())
}

fn sign_wbi(params: &mut BTreeMap<String, String>, mixin_key: &str) -> Result<()> {
    let wts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix epoch")?
        .as_secs();
    params.insert("wts".to_owned(), wts.to_string());
    for value in params.values_mut() {
        value.retain(|character| !"!'()*".contains(character));
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(
        params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let query = serializer.finish();
    let digest = format!("{:x}", md5::compute(format!("{query}{mixin_key}")));
    params.insert("w_rid".to_owned(), digest);
    Ok(())
}

pub fn parse_quality(value: &str, available: &[u32]) -> Result<u32> {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '_'], "");
    if normalized == "best" || normalized == "最高" {
        return available.first().copied().context("没有可用清晰度");
    }
    let id = match normalized.as_str() {
        "240p" => 6,
        "360p" => 16,
        "480p" => 32,
        "720p" => 64,
        "720p60" => 74,
        "1080p" => 80,
        "1080p+" | "1080phigh" => 112,
        "1080p60" => 116,
        "4k" | "2160p" => 120,
        "hdr" => 125,
        "dolby" | "dolbyvision" | "杜比视界" => 126,
        "8k" | "4320p" => 127,
        _ => normalized
            .parse::<u32>()
            .with_context(|| format!("无法识别清晰度 `{value}`"))?,
    };
    if available.contains(&id) {
        Ok(id)
    } else {
        bail!(
            "请求的清晰度 `{value}` 当前不可用；可用 qn：{}",
            available
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

pub fn quality_name(id: u32) -> &'static str {
    match id {
        6 => "240P 极速",
        16 => "360P 流畅",
        32 => "480P 清晰",
        64 => "720P 高清",
        74 => "720P60",
        80 => "1080P 高清",
        112 => "1080P 高码率",
        116 => "1080P60",
        120 => "4K 超高清",
        125 => "HDR 真彩",
        126 => "杜比视界",
        127 => "8K 超高清",
        _ => "未知清晰度",
    }
}

pub fn codec_name(codecid: u32, codecs: &str) -> String {
    match codecid {
        7 => "AVC/H.264".to_owned(),
        12 => "HEVC/H.265".to_owned(),
        13 => "AV1".to_owned(),
        _ if !codecs.is_empty() => codecs.to_owned(),
        _ => format!("codecid={codecid}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PageSelectionSource, PlayData, VideoInfo, VideoPage, WbiImages, extract_mixin_key_enc_tab,
        frontend_script_urls, make_mixin_key, parse_page_selection, parse_quality, parse_video_id,
    };
    use crate::cli::CodecChoice;
    use serde_json::json;
    use url::Url;

    #[test]
    fn selects_video_when_audio_is_null_missing_or_empty() {
        // BV19s4y1G7td returns dash.audio = null for all five silent pages.
        for audio in [None, Some(json!(null)), Some(json!([]))] {
            let mut response = json!({
                "dash": {
                    "video": [{"id": 32, "codecid": 7, "base_url": "https://example.com/video",
                               "backup_url": null}]
                }
            });
            if let Some(audio) = audio {
                response["dash"]["audio"] = audio;
            }
            let play: PlayData = serde_json::from_value(response).unwrap();
            let (video, audio, _) = play.select_streams("best", CodecChoice::Auto).unwrap();
            assert_eq!(video.id, 32);
            assert!(audio.is_none());
            assert_eq!(
                video.urls().collect::<Vec<_>>(),
                ["https://example.com/video"]
            );
        }
    }

    #[test]
    fn keeps_highest_bitrate_audio_and_backup_urls() {
        let play: PlayData = serde_json::from_value(json!({
            "dash": {
                "video": [{"id": 32, "codecid": 7, "base_url": "https://example.com/video",
                           "backup_url": ["https://example.com/backup"]}],
                "audio": [
                    {"id": 1, "base_url": "https://example.com/low", "bandwidth": 64000},
                    {"id": 2, "base_url": "https://example.com/high", "bandwidth": 128000}
                ]
            }
        }))
        .unwrap();
        let (video, audio, _) = play.select_streams("best", CodecChoice::Auto).unwrap();
        assert_eq!(audio.unwrap().id, 2);
        assert_eq!(video.urls().count(), 2);
    }

    #[test]
    fn handles_null_metadata_but_rejects_invalid_list_types() {
        let play: PlayData = serde_json::from_value(json!({
            "accept_quality": null, "accept_description": null, "support_formats": null,
            "dash": {"video": null, "audio": null}
        }))
        .unwrap();
        assert!(play.available_qualities().is_empty());
        assert!(play.select_streams("best", CodecChoice::Auto).is_err());
        for audio in [json!(false), json!("invalid"), json!({})] {
            assert!(
                serde_json::from_value::<PlayData>(json!({
                    "dash": {"audio": audio}
                }))
                .is_err()
            );
        }
    }

    fn video_info(page_count: usize) -> VideoInfo {
        VideoInfo {
            bvid: "BV1xx411c7mD".to_owned(),
            aid: 1,
            title: "demo".to_owned(),
            pages: (1..=page_count)
                .map(|page| VideoPage {
                    cid: page as u64,
                    page,
                    part: format!("part {page}"),
                    duration: 60,
                    bvid: None,
                    ep_id: None,
                })
                .collect(),
        }
    }

    #[test]
    fn parses_bv_from_url() {
        assert!(matches!(
            parse_video_id("https://www.bilibili.com/video/BV1xx411c7mD?p=1").unwrap(),
            super::VideoId::Bvid(value) if value == "BV1xx411c7mD"
        ));
    }

    #[test]
    fn parses_episode_and_season_urls() {
        assert!(matches!(
            parse_video_id("https://www.bilibili.com/bangumi/play/ep693247").unwrap(),
            super::VideoId::Episode(693247)
        ));
        assert!(matches!(
            parse_video_id("ss43164").unwrap(),
            super::VideoId::Season(43164)
        ));
    }

    #[test]
    fn parses_page_numbers_ranges_and_lists() {
        assert_eq!(parse_page_selection("7", 10).unwrap(), vec![7]);
        assert_eq!(
            parse_page_selection("1-7", 10).unwrap(),
            (1..=7).collect::<Vec<_>>()
        );
        assert_eq!(parse_page_selection("5,7", 10).unwrap(), vec![5, 7]);
        assert_eq!(
            parse_page_selection("1-3, 5, 3, 7", 10).unwrap(),
            vec![1, 2, 3, 5, 7]
        );
    }

    #[test]
    fn rejects_invalid_page_selections() {
        assert!(parse_page_selection("", 10).is_err());
        assert!(parse_page_selection("0", 10).is_err());
        assert!(parse_page_selection("7-1", 10).is_err());
        assert!(parse_page_selection("1,,2", 10).is_err());
        assert!(parse_page_selection("11", 10).is_err());
    }

    #[test]
    fn resolves_explicit_url_and_default_page_sources() {
        let info = video_info(8);
        let input = "https://www.bilibili.com/video/BV1pVkrYeEer?spm_id_from=333.788.videopod.sections&vd_source=test&p=7";

        let explicit = info.resolve_pages(input, Some("5,7")).unwrap();
        assert_eq!(explicit.pages, vec![5, 7]);
        assert_eq!(explicit.source, PageSelectionSource::Explicit);

        let from_url = info.resolve_pages(input, None).unwrap();
        assert_eq!(from_url.pages, vec![7]);
        assert_eq!(from_url.source, PageSelectionSource::Url);

        let default = info.resolve_pages("BV1xx411c7mD", None).unwrap();
        assert_eq!(default.pages, vec![1]);
        assert_eq!(default.source, PageSelectionSource::Default);
    }

    #[test]
    fn ignores_page_selection_for_single_page_video() {
        let selection = video_info(1)
            .resolve_pages("https://www.bilibili.com/video/BV1xx411c7mD?p=7", Some("7"))
            .unwrap();
        assert_eq!(selection.pages, vec![1]);
        assert_eq!(selection.source, PageSelectionSource::SinglePage);
    }

    #[test]
    fn parses_quality_aliases() {
        assert_eq!(parse_quality("1080p+", &[120, 112, 80]).unwrap(), 112);
        assert_eq!(parse_quality("best", &[120, 112, 80]).unwrap(), 120);
        assert!(parse_quality("8k", &[80, 64]).is_err());
    }

    #[test]
    fn extracts_frontend_script_urls_in_priority_order() {
        let page_url = Url::parse("https://www.bilibili.com/video/BV1test").unwrap();
        let html = r#"
            <script>
                window.__INITIAL_STATE__ = {
                    "insertScripts": ["\u002F\u002Fs1.hdslb.com\u002Fbfs\u002Fvideo.js"]
                };
            </script>
            <script src='//s1.hdslb.com/bfs/fallback.js'></script>
            <script src='https://example.com/untrusted.js'></script>
        "#;
        let urls = frontend_script_urls(&page_url, html);
        assert_eq!(
            urls.iter().map(Url::as_str).collect::<Vec<_>>(),
            vec![
                "https://s1.hdslb.com/bfs/video.js",
                "https://s1.hdslb.com/bfs/fallback.js"
            ]
        );
    }

    #[test]
    fn extracts_semantic_wbi_permutation_from_frontend_script() {
        let invalid = std::iter::repeat_n("0", 64).collect::<Vec<_>>().join(",");
        let permutation = (0..64)
            .rev()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "const decoy=[{invalid}];function wbi(imgKey,subKey){{\
             const table=[{permutation}];table.forEach(i=>source.charAt(i));\
             return result.slice(0,32)+wts+w_rid;}}"
        );
        assert_eq!(
            extract_mixin_key_enc_tab(&script).unwrap(),
            std::array::from_fn(|index| 63 - index)
        );
    }

    #[test]
    fn builds_mixin_key_with_runtime_permutation() {
        let images = WbiImages {
            img_url: "https://i0.hdslb.com/bfs/wbi/0123456789abcdef0123456789abcdef.png".to_owned(),
            sub_url: "https://i0.hdslb.com/bfs/wbi/fedcba9876543210fedcba9876543210.png".to_owned(),
        };
        let identity = std::array::from_fn(|index| index);
        assert_eq!(
            make_mixin_key(&images, &identity).unwrap(),
            "0123456789abcdef0123456789abcdef"
        );
    }
}
