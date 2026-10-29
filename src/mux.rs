use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};
use mp4::{
    AacConfig, AvcConfig, MediaConfig, MediaType, Mp4Config, Mp4Reader, Mp4Sample, Mp4Writer,
    TrackConfig, TrackType, WriteBox,
};

pub fn mux_avc_aac(video_path: &Path, audio_path: Option<&Path>, output_path: &Path) -> Result<()> {
    mux_streams(Some(video_path), audio_path, output_path, None)
}

#[derive(Clone, Copy)]
pub struct ClipWindow {
    pub start: f64,
    pub end: f64,
}

pub fn mux_clip(
    video: Option<&Path>,
    audio: Option<&Path>,
    output: &Path,
    window: ClipWindow,
) -> Result<()> {
    mux_streams(video, audio, output, Some(window))
}

fn mux_streams(
    video_path: Option<&Path>,
    audio_path: Option<&Path>,
    output_path: &Path,
    window: Option<ClipWindow>,
) -> Result<()> {
    let video = video_path.map(|path| open_mp4(path, "视频")).transpose()?;
    let audio = audio_path.map(|path| open_mp4(path, "音频")).transpose()?;

    let output = File::create(output_path)
        .with_context(|| format!("无法创建临时合并文件 {}", output_path.display()))?;
    let config = Mp4Config {
        major_brand: "isom".parse().expect("static MP4 brand"),
        minor_version: 512,
        compatible_brands: ["isom", "iso2", "avc1", "mp41"]
            .into_iter()
            .map(|brand| brand.parse().expect("static MP4 brand"))
            .collect(),
        timescale: 1000,
    };
    let mut writer =
        Mp4Writer::write_start(BufWriter::new(output), &config).context("初始化 MP4 封装器失败")?;
    if let Some((video, video_path)) = video.as_ref().zip(video_path) {
        let (track_id, config) = video_track_config(video)?;
        writer.add_track(&config).context("创建视频轨道失败")?;
        copy_fragmented_samples(video, video_path, track_id, &mut writer, 1, "视频", window)?;
    }
    if let Some((audio, audio_path)) = audio.as_ref().zip(audio_path) {
        let (audio_track_id, audio_config) = audio_track_config(audio)?;
        writer
            .add_track(&audio_config)
            .context("创建音频轨道失败")?;
        let destination_id = if video.is_some() { 2 } else { 1 };
        copy_fragmented_samples(
            audio,
            audio_path,
            audio_track_id,
            &mut writer,
            destination_id,
            "音频",
            window,
        )?;
    }
    writer.write_end().context("写入 MP4 索引失败")?;
    writer
        .into_writer()
        .flush()
        .context("刷新 MP4 合并文件失败")?;
    if let Some(window) = window {
        limit_presentation_duration(output_path, window.end - window.start)?;
    }
    Ok(())
}

// Keep reference frames needed for B-frame decoding, but hide presentations past
// the requested end using a standard MP4 edit list. Media packets stay unchanged.
fn limit_presentation_duration(path: &Path, seconds: f64) -> Result<()> {
    let mut moov = open_mp4(path, "输出")?.moov;
    let offset = top_level_box_offsets(path, *b"moov")?
        .into_iter()
        .last()
        .context("输出缺少 moov")?;
    let duration = (seconds * f64::from(moov.mvhd.timescale)).round() as u64;
    let mut movie_duration = 0;
    for track in &mut moov.traks {
        let media_duration = (u128::from(track.mdia.mdhd.duration)
            * u128::from(moov.mvhd.timescale)
            / u128::from(track.mdia.mdhd.timescale)) as u64;
        let duration = duration.min(media_duration);
        movie_duration = movie_duration.max(duration);
        track.tkhd.duration = duration;
        let edit = track
            .edts
            .get_or_insert_with(Default::default)
            .elst
            .get_or_insert_with(Default::default);
        edit.version = 1;
        edit.entries.clear();
        edit.entries.push(Default::default());
        edit.entries[0].segment_duration = duration;
        edit.entries[0].media_time = 0;
        edit.entries[0].media_rate = 1;
    }
    moov.mvhd.duration = movie_duration;
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
    moov.write_box(&mut file)?;
    file.flush()?;
    Ok(())
}

fn open_mp4(path: &Path, label: &str) -> Result<Mp4Reader<BufReader<File>>> {
    let file = File::open(path).with_context(|| format!("无法打开{label}流 {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("无法读取{label}流大小"))?
        .len();
    Mp4Reader::read_header(BufReader::new(file), size)
        .with_context(|| format!("解析{label} M4S 失败"))
}

fn video_track_config<R: std::io::Read + std::io::Seek>(
    reader: &Mp4Reader<R>,
) -> Result<(u32, TrackConfig)> {
    let (track_id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().ok() == Some(TrackType::Video))
        .context("视频 M4S 中没有视频轨道")?;
    if track.media_type().context("识别视频编码失败")? != MediaType::H264 {
        bail!("内置 MP4 合并目前仅支持 AVC/H.264 视频；请使用 --codec avc");
    }
    let config = AvcConfig {
        width: track.width(),
        height: track.height(),
        seq_param_set: track
            .sequence_parameter_set()
            .context("读取 H.264 SPS 失败")?
            .to_vec(),
        pic_param_set: track
            .picture_parameter_set()
            .context("读取 H.264 PPS 失败")?
            .to_vec(),
    };
    Ok((
        *track_id,
        TrackConfig {
            track_type: TrackType::Video,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            media_conf: MediaConfig::AvcConfig(config),
        },
    ))
}

fn audio_track_config<R: std::io::Read + std::io::Seek>(
    reader: &Mp4Reader<R>,
) -> Result<(u32, TrackConfig)> {
    let (track_id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().ok() == Some(TrackType::Audio))
        .context("音频 M4S 中没有音频轨道")?;
    if track.media_type().context("识别音频编码失败")? != MediaType::AAC {
        bail!("内置 MP4 合并目前仅支持 AAC 音频");
    }
    let config = AacConfig {
        bitrate: track.bitrate().max(128_000),
        profile: track.audio_profile().context("读取 AAC Profile 失败")?,
        freq_index: track.sample_freq_index().context("读取 AAC 采样率失败")?,
        chan_conf: track.channel_config().context("读取 AAC 声道配置失败")?,
    };
    Ok((
        *track_id,
        TrackConfig {
            track_type: TrackType::Audio,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            media_conf: MediaConfig::AacConfig(config),
        },
    ))
}

fn copy_fragmented_samples<R: Read + Seek, W: Write + Seek>(
    reader: &Mp4Reader<R>,
    source_path: &Path,
    source_track_id: u32,
    writer: &mut Mp4Writer<W>,
    destination_track_id: u32,
    label: &str,
    window: Option<ClipWindow>,
) -> Result<()> {
    if !reader.is_fragmented() {
        bail!("{label}输入不是 fragmented MP4");
    }
    let moof_offsets = top_level_box_offsets(source_path, *b"moof")?;
    if moof_offsets.len() != reader.moofs.len() {
        bail!("{label} M4S 的 moof 数量与解析结果不一致");
    }
    let trex = reader
        .moov
        .mvex
        .as_ref()
        .map(|mvex| &mvex.trex)
        .filter(|trex| trex.track_id == source_track_id);
    let mut source = BufReader::new(
        File::open(source_path)
            .with_context(|| format!("无法重新打开{label}流 {}", source_path.display()))?,
    );
    let mut total_samples = 0u64;
    let timescale = f64::from(
        reader
            .tracks()
            .get(&source_track_id)
            .context("轨道不存在")?
            .timescale(),
    );
    let origin = window
        .map(|w| (w.start * timescale).round() as u64)
        .unwrap_or(0);
    let limit = window
        .map(|w| (w.end * timescale).round() as u64)
        .unwrap_or(u64::MAX);

    for (moof_index, moof) in reader.moofs.iter().enumerate() {
        let moof_start = moof_offsets[moof_index];
        for traf in moof
            .trafs
            .iter()
            .filter(|traf| traf.tfhd.track_id == source_track_id)
        {
            let trun = traf
                .trun
                .as_ref()
                .with_context(|| format!("{label} traf 中缺少 trun"))?;
            let base_offset = traf.tfhd.base_data_offset.unwrap_or(moof_start);
            if window.is_some() && traf.tfhd.base_data_offset.is_some() {
                bail!("片段使用绝对媒体偏移，暂不支持该流的时间段封装");
            }
            let mut sample_offset = add_signed_offset(base_offset, trun.data_offset.unwrap_or(0))
                .with_context(|| format!("{label} trun 数据偏移无效"))?;
            let mut decode_time = traf
                .tfdt
                .as_ref()
                .map(|tfdt| tfdt.base_media_decode_time)
                .unwrap_or(0);

            for sample_index in 0..trun.sample_count as usize {
                let sample_size = trun
                    .sample_sizes
                    .get(sample_index)
                    .copied()
                    .or(traf.tfhd.default_sample_size)
                    .or_else(|| trex.map(|trex| trex.default_sample_size))
                    .filter(|size| *size > 0)
                    .with_context(|| format!("{label}样本缺少大小"))?;
                let duration = trun
                    .sample_durations
                    .get(sample_index)
                    .copied()
                    .or(traf.tfhd.default_sample_duration)
                    .or_else(|| trex.map(|trex| trex.default_sample_duration))
                    .filter(|duration| *duration > 0)
                    .with_context(|| format!("{label}样本缺少时长"))?;
                let sample_flags = trun
                    .sample_flags
                    .get(sample_index)
                    .copied()
                    .or_else(|| {
                        (sample_index == 0)
                            .then_some(trun.first_sample_flags)
                            .flatten()
                    })
                    .or(traf.tfhd.default_sample_flags)
                    .or_else(|| trex.map(|trex| trex.default_sample_flags))
                    .unwrap_or(0);
                let rendering_offset = trun
                    .sample_cts
                    .get(sample_index)
                    .copied()
                    .map(|offset| {
                        if trun.version == 1 {
                            offset as i32
                        } else {
                            i32::try_from(offset).unwrap_or(i32::MAX)
                        }
                    })
                    .unwrap_or(0);

                if window.is_some() && (decode_time < origin || decode_time >= limit) {
                    sample_offset += u64::from(sample_size);
                    decode_time += u64::from(duration);
                    continue;
                }
                if window.is_some()
                    && label == "视频"
                    && total_samples == 0
                    && sample_flags & 0x0001_0000 != 0
                {
                    bail!("时间段首帧不是关键帧，无法安全无损封装");
                }
                let mut bytes = vec![0u8; sample_size as usize];
                source
                    .seek(SeekFrom::Start(sample_offset))
                    .and_then(|_| source.read_exact(&mut bytes))
                    .with_context(|| {
                        format!(
                            "读取{label}样本 {}（偏移 {sample_offset}，大小 {sample_size}）失败",
                            total_samples + 1
                        )
                    })?;
                let sample = Mp4Sample {
                    start_time: decode_time.saturating_sub(origin),
                    duration: u64::from(duration).min(limit - decode_time) as u32,
                    rendering_offset,
                    is_sync: sample_flags & 0x0001_0000 == 0,
                    bytes: bytes.into(),
                };
                writer
                    .write_sample(destination_track_id, &sample)
                    .with_context(|| format!("写入{label}样本 {} 失败", total_samples + 1))?;
                sample_offset += u64::from(sample_size);
                decode_time += u64::from(duration);
                total_samples += 1;
            }
        }
    }
    if total_samples == 0 {
        bail!("{label}轨道没有媒体样本");
    }
    Ok(())
}

fn add_signed_offset(base: u64, offset: i32) -> Option<u64> {
    if offset >= 0 {
        base.checked_add(offset as u64)
    } else {
        base.checked_sub(offset.unsigned_abs() as u64)
    }
}

fn top_level_box_offsets(path: &Path, target: [u8; 4]) -> Result<Vec<u64>> {
    let mut file = BufReader::new(
        File::open(path).with_context(|| format!("无法扫描 MP4 盒结构 {}", path.display()))?,
    );
    let file_size = file.get_ref().metadata()?.len();
    let mut offsets = Vec::new();
    let mut position = 0u64;
    while position + 8 <= file_size {
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let short_size = u32::from_be_bytes(header[0..4].try_into().expect("four bytes"));
        let box_type: [u8; 4] = header[4..8].try_into().expect("four bytes");
        let box_size = match short_size {
            0 => file_size - position,
            1 => {
                let mut extended = [0u8; 8];
                file.read_exact(&mut extended)?;
                u64::from_be_bytes(extended)
            }
            size => u64::from(size),
        };
        let header_size = if short_size == 1 { 16 } else { 8 };
        if box_size < header_size || position + box_size > file_size {
            bail!("{} 包含无效的顶层 MP4 盒大小", path.display());
        }
        if box_type == target {
            offsets.push(position);
        }
        position += box_size;
    }
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::{add_signed_offset, limit_presentation_duration, open_mp4};
    use mp4::{AacConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};
    use std::{
        fs::File,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn edit_list_uses_media_duration_without_per_sample_rounding_loss() {
        let path = std::env::temp_dir().join(format!(
            "bildowner-edit-{}-{}.mp4",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = Mp4Config {
            major_brand: "isom".parse().unwrap(),
            minor_version: 0,
            compatible_brands: vec![],
            timescale: 1000,
        };
        let mut writer = Mp4Writer::write_start(File::create(&path).unwrap(), &config).unwrap();
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: 48000,
                language: "und".to_owned(),
                media_conf: MediaConfig::AacConfig(AacConfig::default()),
            })
            .unwrap();
        for index in 0..10 {
            writer
                .write_sample(
                    1,
                    &Mp4Sample {
                        start_time: index * 1024,
                        duration: 1024,
                        rendering_offset: 0,
                        is_sync: true,
                        bytes: vec![0; 8].into(),
                    },
                )
                .unwrap();
        }
        writer.write_end().unwrap();
        drop(writer);
        limit_presentation_duration(&path, 1.0).unwrap();
        let reader = open_mp4(&path, "测试").unwrap();
        let track = &reader.moov.traks[0];
        assert_eq!(reader.moov.mvhd.duration, 213);
        assert_eq!(
            track.edts.as_ref().unwrap().elst.as_ref().unwrap().entries[0].segment_duration,
            213
        );
        assert_eq!(reader.sample_count(1).unwrap(), 10);
        drop(reader);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn applies_signed_fragment_offsets() {
        assert_eq!(add_signed_offset(100, 24), Some(124));
        assert_eq!(add_signed_offset(100, -24), Some(76));
        assert_eq!(add_signed_offset(10, -24), None);
    }
}
