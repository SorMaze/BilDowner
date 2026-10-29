use std::{
    io::{Cursor, Read},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use indicatif::MultiProgress;
use reqwest::{
    StatusCode,
    header::{CONTENT_RANGE, RANGE},
};
use tokio::{fs, io::AsyncWriteExt};

use crate::{
    api::{BiliClient, DashStream},
    http_download::make_progress_bar,
    time_range::TimeRange,
};
use crate::{
    cli::DownloadArgs,
    mux::{self, ClipWindow},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    fn parse(value: &str) -> Result<Self> {
        let (start, end) = value.split_once('-').context("DASH 字节区间不合法")?;
        let result = Self {
            start: start.parse()?,
            end: end.parse()?,
        };
        ensure!(
            result.start <= result.end && result.end < u64::MAX,
            "DASH 字节区间不合法"
        );
        Ok(result)
    }

    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

#[derive(Debug)]
struct Segment {
    bytes: ByteRange,
    start: f64,
    end: f64,
    keyframe: bool,
}

pub struct ClipPlan {
    initialization: Vec<u8>,
    pub bytes: ByteRange,
    pub start: f64,
    pub end: f64,
    total_size: u64,
}

fn u32_be(reader: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).context("SIDX 索引被截断")?;
    Ok(u32::from_be_bytes(bytes))
}

fn u64_be(reader: &mut Cursor<&[u8]>) -> Result<u64> {
    Ok((u64::from(u32_be(reader)?) << 32) | u64::from(u32_be(reader)?))
}

fn parse_sidx(data: &[u8], offset: u64) -> Result<Vec<Segment>> {
    let mut reader = Cursor::new(data);
    let size = u64::from(u32_be(&mut reader)?);
    ensure!(
        u32_be(&mut reader)? == u32::from_be_bytes(*b"sidx"),
        "DASH 索引不是 SIDX"
    );
    let size = if size == 1 {
        u64_be(&mut reader)?
    } else {
        size
    };
    ensure!(size == data.len() as u64, "SIDX 长度与索引区间不一致");
    let version = u32_be(&mut reader)? >> 24;
    ensure!(version <= 1, "不支持的 SIDX 版本 {version}");
    let _track = u32_be(&mut reader)?;
    let timescale = u32_be(&mut reader)?;
    ensure!(timescale > 0, "SIDX timescale 不能为 0");
    let (mut timestamp, first_offset) = if version == 0 {
        (
            u64::from(u32_be(&mut reader)?),
            u64::from(u32_be(&mut reader)?),
        )
    } else {
        (u64_be(&mut reader)?, u64_be(&mut reader)?)
    };
    let count = u32_be(&mut reader)? & 0xffff;
    let earliest_time = timestamp;
    ensure!(
        reader.position() + u64::from(count) * 12 == size,
        "SIDX 条目数量不合法"
    );
    let mut position = offset
        .checked_add(size)
        .and_then(|v| v.checked_add(first_offset))
        .context("SIDX 偏移溢出")?;
    let mut segments = Vec::new();
    for _ in 0..count {
        let reference = u32_be(&mut reader)?;
        ensure!(reference >> 31 == 0, "暂不支持嵌套 SIDX 索引");
        let length = u64::from(reference & 0x7fff_ffff);
        let duration = u64::from(u32_be(&mut reader)?);
        let sap = u32_be(&mut reader)?;
        ensure!(length > 0 && duration > 0, "SIDX 条目大小或时长为 0");
        let next_time = timestamp.checked_add(duration).context("SIDX 时间溢出")?;
        let next_position = position.checked_add(length).context("SIDX 偏移溢出")?;
        segments.push(Segment {
            bytes: ByteRange {
                start: position,
                end: next_position - 1,
            },
            // User timestamps are relative to the beginning of the track, not
            // the encoder's earliest_presentation_time (which may include delay).
            start: (timestamp - earliest_time) as f64 / f64::from(timescale),
            end: (next_time - earliest_time) as f64 / f64::from(timescale),
            keyframe: sap >> 31 == 1 && ((sap >> 28) & 7) <= 2 && sap & 0x0fff_ffff == 0,
        });
        timestamp = next_time;
        position = next_position;
    }
    Ok(segments)
}

fn select_segments(segments: &[Segment], start: f64, end: f64) -> Result<(ByteRange, f64, f64)> {
    let first = segments
        .iter()
        .position(|s| s.end > start)
        .context("时间起点超出了该分 P 的实际时长")?;
    let first = (0..=first)
        .rev()
        .find(|&i| segments[i].keyframe)
        .context("时间起点之前没有可独立解码的关键帧分段")?;
    let last = segments
        .iter()
        .rposition(|s| s.start < end)
        .context("时间终点早于媒体起点")?;
    ensure!(last >= first, "时间区间没有媒体片段");
    Ok((
        ByteRange {
            start: segments[first].bytes.start,
            end: segments[last].bytes.end,
        },
        segments[first].start,
        end.min(segments[last].end),
    ))
}

async fn range_response(
    request: reqwest::RequestBuilder,
    bytes: ByteRange,
) -> Result<(reqwest::Response, u64)> {
    let response = request
        .timeout(std::time::Duration::from_secs(120))
        .header(RANGE, format!("bytes={}-{}", bytes.start, bytes.end))
        .send()
        .await
        .context("连接片段下载地址失败")?;
    ensure!(
        response.status() == StatusCode::PARTIAL_CONTENT,
        "CDN 未接受片段 Range 请求（HTTP {}），不会回退到完整下载",
        response.status()
    );
    let header = response
        .headers()
        .get(CONTENT_RANGE)
        .context("CDN 没有返回 Content-Range")?
        .to_str()?;
    let (range, total) = header
        .strip_prefix("bytes ")
        .and_then(|v| v.split_once('/'))
        .context("Content-Range 不合法")?;
    ensure!(
        ByteRange::parse(range)? == bytes,
        "CDN 返回了错误的字节区间"
    );
    let total: u64 = total.parse().context("CDN 未返回完整媒体大小")?;
    ensure!(total > bytes.end, "Content-Range 总大小不合法");
    if let Some(length) = response.content_length() {
        ensure!(length == bytes.len(), "CDN 响应长度不匹配");
    }
    Ok((response, total))
}

pub async fn plan(
    client: &BiliClient,
    stream: &DashStream,
    referer: &str,
    time: TimeRange,
) -> Result<ClipPlan> {
    let base = stream
        .segment_base
        .as_ref()
        .or(stream.legacy_segment_base.as_ref())
        .context("接口没有提供 DASH 分段索引，无法按时间部分下载")?;
    let init = ByteRange::parse(&base.initialization)?;
    let index = ByteRange::parse(&base.index_range)?;
    ensure!(
        init.start == 0 && init.end < index.start && index.end < 4 * 1024 * 1024,
        "不支持的 DASH 初始化或索引区间"
    );
    let header_range = ByteRange {
        start: 0,
        end: index.end,
    };
    let mut last_error = None;
    for url in stream.urls() {
        let attempt = async {
            let (response, total_size) =
                range_response(client.media_get(url, referer), header_range).await?;
            let mut data = Vec::new();
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk?;
                ensure!(
                    data.len() as u64 + chunk.len() as u64 <= header_range.len(),
                    "索引响应超出请求大小"
                );
                data.extend_from_slice(&chunk);
            }
            ensure!(
                data.len() as u64 == header_range.len(),
                "DASH 索引下载不完整"
            );
            let segments = parse_sidx(&data[index.start as usize..], index.start)?;
            ensure!(
                segments.last().is_some_and(|s| s.bytes.end < total_size),
                "SIDX 指向媒体范围之外"
            );
            let (bytes, start, end) = select_segments(&segments, time.start(), time.end())?;
            Ok::<_, anyhow::Error>(ClipPlan {
                initialization: data[..=init.end as usize].to_vec(),
                bytes,
                start,
                end,
                total_size,
            })
        }
        .await;
        match attempt {
            Ok(plan) => return Ok(plan),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有可用的分段索引地址")))
}

pub async fn download(
    client: &BiliClient,
    stream: &DashStream,
    referer: &str,
    plan: &ClipPlan,
    destination: &Path,
    progress: &MultiProgress,
    label: &str,
) -> Result<()> {
    if destination.exists() {
        fs::remove_file(destination).await?;
    }
    let partial = destination.with_extension("m4s.part");
    let mut last_error = None;
    for url in stream.urls() {
        let attempt = async {
            let (response, total) =
                range_response(client.media_get(url, referer), plan.bytes).await?;
            ensure!(total == plan.total_size, "备用 CDN 媒体大小与索引不一致");
            let bar = progress.add(make_progress_bar(label, Some(plan.bytes.len())));
            let result = async {
                let mut file = fs::File::create(&partial).await?;
                file.write_all(&plan.initialization).await?;
                let mut body = response.bytes_stream();
                let mut received = 0u64;
                while let Some(chunk) = body.next().await {
                    let chunk = chunk?;
                    received = received
                        .checked_add(chunk.len() as u64)
                        .context("片段大小溢出")?;
                    ensure!(received <= plan.bytes.len(), "CDN 返回了超出区间的数据");
                    file.write_all(&chunk).await?;
                    bar.inc(chunk.len() as u64);
                }
                ensure!(received == plan.bytes.len(), "片段下载不完整");
                file.flush().await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            bar.finish_and_clear();
            result?;
            fs::rename(&partial, destination).await?;
            progress.suspend(|| {
                println!(
                    "{label}片段下载完成：{} 字节 / 完整流 {} 字节",
                    plan.bytes.len(),
                    total
                )
            });
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match attempt {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    bail!("片段下载失败：{}", last_error.context("没有可用下载地址")?);
}

pub async fn run(
    client: &BiliClient,
    video: &DashStream,
    audio: Option<&DashStream>,
    referer: &str,
    base: &str,
    args: &DownloadArgs,
) -> Result<()> {
    ensure!(
        video.codecid == 7,
        "-t 当前的纯 Rust 无损封装支持 AVC/H.264，请使用 --codec avc"
    );
    let time = args.time.context("缺少时间区间")?;
    let base = format!("{base}{}", time.suffix());
    let video_output = args.output_dir.join(format!("{base}.video.mp4"));
    let audio_output = args.output_dir.join(format!("{base}.audio.m4a"));
    let merged_output = args.output_dir.join(format!("{base}.mp4"));
    let write_merged = args.mode.needs_merge() && (args.force || !merged_output.exists());
    let write_video = args.mode.keeps_separate() && (args.force || !video_output.exists());
    let write_audio =
        args.mode.keeps_separate() && audio.is_some() && (args.force || !audio_output.exists());
    if !write_merged && !write_video && !write_audio {
        println!("该时间段的输出文件已存在，跳过：{base}");
        return Ok(());
    }
    let video_plan = plan(client, video, referer, time).await?;
    let window = ClipWindow {
        start: video_plan.start,
        end: video_plan.end,
    };
    println!(
        "请求时间段：{time}\n无损关键帧对齐：实际起点 {:.3} 秒，终点 {:.3} 秒（按媒体帧边界结束）；只下载所需分段。",
        window.start, window.end
    );
    let audio_time = TimeRange {
        start_ms: (window.start * 1000.0).round() as u64,
        end_ms: (window.end * 1000.0).round() as u64,
    };
    let audio_plan = if let Some(audio) = audio.filter(|_| write_merged || write_audio) {
        Some(plan(client, audio, referer, audio_time).await?)
    } else {
        None
    };
    let video_raw = args.output_dir.join(format!("{base}.clip.video.m4s"));
    let audio_raw = args.output_dir.join(format!("{base}.clip.audio.m4s"));
    let progress = MultiProgress::new();
    let video_download = async {
        if write_merged || write_video {
            download(
                client,
                video,
                referer,
                &video_plan,
                &video_raw,
                &progress,
                "视频",
            )
            .await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let audio_download = async {
        if let Some((audio, plan)) = audio.zip(audio_plan.as_ref()) {
            download(client, audio, referer, plan, &audio_raw, &progress, "音频").await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(video_download, audio_download)?;
    let audio_raw_option = audio.map(|_| audio_raw.clone());
    let video_for_task = video_raw.clone();
    let audio_for_task = audio_raw_option.clone();
    let outputs = [
        (write_merged, merged_output.clone(), true, audio.is_some()),
        (write_video, video_output.clone(), true, false),
        (write_audio, audio_output.clone(), false, true),
    ];
    tokio::task::spawn_blocking(move || {
        for (needed, output, has_video, has_audio) in outputs {
            if !needed {
                continue;
            }
            let temporary = output.with_extension("tmp.mp4");
            let result = mux::mux_clip(
                has_video.then_some(video_for_task.as_path()),
                if has_audio {
                    audio_for_task.as_deref()
                } else {
                    None
                },
                &temporary,
                window,
            );
            if let Err(error) = result {
                std::fs::remove_file(&temporary).ok();
                return Err(error);
            }
            if output.exists() {
                std::fs::remove_file(&output)?;
            }
            std::fs::rename(&temporary, &output)?;
            println!("时间段文件：{}", output.display());
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("时间段封装任务异常结束")??;
    if video_raw.exists() {
        fs::remove_file(video_raw).await?;
    }
    if audio_raw.exists() {
        fs::remove_file(audio_raw).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, parse_sidx, range_response, select_segments};
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    fn sidx(version: u32, earliest: u64) -> Vec<u8> {
        let mut words = vec![
            32 + version * 8 + 5 * 12,
            u32::from_be_bytes(*b"sidx"),
            version << 24,
            1,
            1000,
        ];
        if version == 1 {
            words.extend([(earliest >> 32) as u32, earliest as u32, 0, 7]);
        } else {
            words.extend([earliest as u32, 7]);
        }
        words.push(5);
        for _ in 0..5 {
            words.extend([100, 5000, 0x9000_0000]);
        }
        words.into_iter().flat_map(u32::to_be_bytes).collect()
    }

    #[test]
    fn uses_partial_byte_ranges_and_normalizes_encoder_delay() {
        for (version, earliest) in [(0, 21), (1, 1u64 << 33)] {
            let data = sidx(version, earliest);
            let segments = parse_sidx(&data, 1000).unwrap();
            let (bytes, start, end) = select_segments(&segments, 17.0, 24.0).unwrap();
            assert_eq!(start, 15.0);
            assert_eq!(end, 24.0);
            assert_eq!(bytes.start, 1000 + data.len() as u64 + 7 + 300);
            assert_eq!(bytes.len(), 200);
            let (_, start, end) = select_segments(&segments, 5.0, 10.0).unwrap();
            assert_eq!((start, end), (5.0, 10.0));
            assert!(select_segments(&segments, 25.0, 26.0).is_err());
            assert_eq!(select_segments(&segments, 24.0, 100.0).unwrap().2, 25.0);
        }
    }

    #[test]
    fn falls_back_to_previous_independent_segment() {
        let mut segments = parse_sidx(&sidx(0, 0), 0).unwrap();
        segments[3].keyframe = false;
        assert_eq!(select_segments(&segments, 17.0, 24.0).unwrap().1, 10.0);
        segments.iter_mut().for_each(|s| s.keyframe = false);
        assert!(select_segments(&segments, 17.0, 24.0).is_err());
    }

    #[test]
    fn rejects_truncated_nested_and_invalid_sidx() {
        let data = sidx(0, 0);
        for length in 0..data.len() {
            assert!(parse_sidx(&data[..length], 0).is_err());
        }
        let mut nested = data.clone();
        nested[32..36].copy_from_slice(&0x8000_0064u32.to_be_bytes());
        assert!(parse_sidx(&nested, 0).is_err());
        let mut zero_timescale = data.clone();
        zero_timescale[16..20].fill(0);
        assert!(parse_sidx(&zero_timescale, 0).is_err());
        assert!(parse_sidx(&data, u64::MAX).is_err());
    }

    #[tokio::test]
    async fn requires_exact_partial_content_without_full_download_fallback() {
        for (status, content_range, accepted) in [
            (206, "bytes 100-199/1000", true),
            (200, "bytes 100-199/1000", false),
            (206, "bytes 0-99/1000", false),
            (206, "bytes 100-199/*", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|v| v == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                assert!(
                    String::from_utf8(request)
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("range: bytes=100-199")
                );
                let header = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Range: {content_range}\r\nContent-Length: 100\r\nConnection: close\r\n\r\n"
                );
                socket.write_all(header.as_bytes()).unwrap();
                let _ = socket.write_all(&[0; 100]);
            });
            let result = range_response(
                reqwest::Client::new().get(format!("http://{address}")),
                ByteRange {
                    start: 100,
                    end: 199,
                },
            )
            .await;
            assert_eq!(result.is_ok(), accepted);
            if let Ok((response, total)) = result {
                assert_eq!(total, 1000);
                assert_eq!(response.bytes().await.unwrap().len(), 100);
            }
            server.join().unwrap();
        }
    }
}
