// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use crate::{Error, PathInfo, Result, ResultExt, Version};
use async_stream::try_stream;
use chrono::DateTime;
use futures::future::OptionFuture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_stream::{Stream, StreamExt};

#[derive(Debug)]
pub struct NixReader<R: AsyncReadExt + Unpin> {
    r: R,
    proto: Version,
}

impl<R: AsyncReadExt + Unpin> NixReader<R> {
    pub fn new(r: R, proto: Version) -> Self {
        Self { r, proto }
    }

    /// Read a u64 from the stream (little endian).
    pub async fn read_u64(&mut self) -> Result<u64> {
        Ok(self.r.read_u64_le().await?)
    }

    /// Read a boolean from the stream, encoded as u64 (>0 is true).
    pub async fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_u64().await? > 0)
    }

    /// Reads a string from the stream. Strings are prefixed with a u64 length, but the
    /// data is padded to the next 8-byte boundary, eg. a 1-byte string becomes 16 bytes
    /// on the wire: 8 for the length, 1 for the data, then 7 bytes of discarded 0x00s.
    pub async fn read_string(&mut self) -> Result<String> {
        let len = self.read_u64().await.with_field("<length>")? as usize;
        let padded_len = len + if len % 8 > 0 { 8 - (len % 8) } else { 0 };
        if padded_len <= 1024 {
            let mut buf = [0u8; 1024];
            self.r.read_exact(&mut buf[..padded_len]).await?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            let mut buf = vec![0u8; padded_len];
            self.r.read_exact(&mut buf[..padded_len]).await?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        }
    }

    /// Reads a list (or set) of strings from the stream - a u64 count, followed by that
    /// many strings using the normal `read_string()` encoding.
    pub fn read_strings(&mut self) -> impl Stream<Item = Result<String>> + '_ {
        try_stream! {
            let count = self.read_u64().await.with_field("<count>")? as usize;
            for _ in 0..count {
                yield self.read_string().await?;
            }
        }
    }

    /// Reads a PathInfo structure from the stream.
    pub async fn read_pathinfo(&mut self) -> Result<PathInfo> {
        let deriver = self
            .read_string()
            .await
            .map(|s| (!s.is_empty()).then_some(s)) // "" -> None.
            .with_field("PathInfo.deriver")?;
        let nar_hash = self.read_string().await.with_field("PathInfo.nar_hash")?;
        let references = self
            .read_strings()
            .collect::<Result<Vec<_>>>()
            .await
            .with_field("PathInfo.deriver")?;
        let registration_time = self
            .read_u64()
            .await
            .with_field("PathInfo.registration_time")
            .and_then(|ts| {
                DateTime::from_timestamp(ts as i64, 0).ok_or_else(|| Error::Invalid(ts.to_string()))
            })?;
        let nar_size = self.read_u64().await.with_field("PathInfo.nar_size")?;

        let ultimate = OptionFuture::from(self.proto.since(16).then(|| self.read_bool()))
            .await
            .transpose()
            .with_field("PathInfo.ultimate")?
            .unwrap_or_default();
        let signatures =
            OptionFuture::from(self.proto.since(16).then(|| self.read_strings().collect()))
                .await
                .transpose()
                .with_field("PathInfo.signatures")?
                .unwrap_or_default();
        let ca = OptionFuture::from(self.proto.since(16).then(|| self.read_string()))
            .await
            .transpose()
            .with_field("PathInfo.ca")?
            .and_then(|s| (!s.is_empty()).then_some(s)); // "" -> None.

        Ok(PathInfo {
            deriver,
            nar_hash,
            references,
            registration_time,
            nar_size,
            ultimate,
            signatures,
            ca,
        })
    }
}

#[derive(Debug)]
pub struct NixWriter<W: AsyncWriteExt + Unpin> {
    w: W,
    proto: Version,
}

impl<W: AsyncWriteExt + Unpin> NixWriter<W> {
    pub fn new(w: W, proto: Version) -> Self {
        Self { w, proto }
    }

    /// Write a u64 from the stream (little endian).
    pub async fn write_u64(&mut self, v: u64) -> Result<()> {
        Ok(self.w.write_u64_le(v).await?)
    }

    /// Write a boolean from the stream, encoded as u64 (>0 is true).
    pub async fn write_bool(&mut self, v: bool) -> Result<()> {
        Ok(self.write_u64(v.then_some(1u64).unwrap_or(0u64)).await?)
    }

    /// Write a string to the stream. See: NixReader::read_string.
    pub async fn write_string<S: AsRef<str>>(&mut self, s: S) -> Result<()> {
        let b = s.as_ref().as_bytes();
        self.write_u64(b.len().try_into().unwrap())
            .await
            .with_field("<length>")?;
        if b.len() > 0 {
            self.w.write_all(b).await?;
            if b.len() % 8 > 0 {
                let pad_buf = [0u8; 7];
                self.w.write_all(&pad_buf[..8 - (b.len() % 8)]).await?;
            }
        }
        Ok(())
    }

    /// Write a list of strings to the stream. See: NixReader::read_strings.
    pub async fn write_strings<S: AsRef<str>>(&mut self, sl: &[S]) -> Result<()> {
        self.write_u64(sl.len().try_into().unwrap())
            .await
            .with_field("<count>")?;
        for s in sl {
            self.write_string(s.as_ref()).await?;
        }
        Ok(())
    }

    pub async fn write_pathinfo(&mut self, pi: &PathInfo) -> Result<()> {
        self.write_string(pi.deriver.as_ref().map(|s| s.as_str()).unwrap_or(""))
            .await
            .with_field("PathInfo.deriver")?;
        self.write_string(pi.nar_hash.as_str())
            .await
            .with_field("PathInfo.nar_hash")?;
        self.write_strings(&pi.references)
            .await
            .with_field("PathInfo.deriver")?;
        self.write_u64(pi.registration_time.timestamp().try_into().unwrap())
            .await
            .with_field("PathInfo.registration_time")?;
        self.write_u64(pi.nar_size)
            .await
            .with_field("PathInfo.nar_size")?;

        if self.proto.since(16) {
            self.write_bool(pi.ultimate)
                .await
                .with_field("PathInfo.ultimate")?;
            self.write_strings(&pi.signatures)
                .await
                .with_field("PathInfo.signatures")?;
            self.write_string(&pi.ca.as_ref().map(|s| s.as_str()).unwrap_or(""))
                .await
                .with_field("PathInfo.ca")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use tokio_stream::StreamExt;
    use tokio_test::io::Builder;

    fn pad_str<const L: usize>(s: &str) -> [u8; L] {
        assert!(L % 8 == 0, "{} is not aligned to 8", L);
        let mut v = [0u8; L];
        (&mut v[..s.len()]).copy_from_slice(s.as_bytes());
        v
    }

    // Integers.
    #[tokio::test]
    async fn test_read_u64() {
        let mock = Builder::new().read(&1234567890u64.to_le_bytes()).build();
        assert_eq!(
            1234567890u64,
            NixReader::new(mock, Version(0, 0))
                .read_u64()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_write_u64() {
        let mock = Builder::new().write(&1234567890u64.to_le_bytes()).build();
        NixWriter::new(mock, Version(0, 0))
            .write_u64(1234567890)
            .await
            .unwrap();
    }

    // Booleans.
    #[tokio::test]
    async fn test_read_bool_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!(
            false,
            NixReader::new(mock, Version(0, 0))
                .read_bool()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_bool_1() {
        let mock = Builder::new().read(&1u64.to_le_bytes()).build();
        assert_eq!(
            true,
            NixReader::new(mock, Version(0, 0))
                .read_bool()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_bool_2() {
        let mock = Builder::new().read(&2u64.to_le_bytes()).build();
        assert_eq!(
            true,
            NixReader::new(mock, Version(0, 0))
                .read_bool()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_write_bool_false() {
        let mock = Builder::new().write(&0u64.to_le_bytes()).build();
        NixWriter::new(mock, Version(0, 0))
            .write_bool(false)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn test_write_bool_true() {
        let mock = Builder::new().write(&1u64.to_le_bytes()).build();
        NixWriter::new(mock, Version(0, 0))
            .write_bool(true)
            .await
            .unwrap();
    }

    // Short strings.
    #[tokio::test]
    async fn test_read_string_len_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!(
            "".to_string(),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_1() {
        let mock = Builder::new()
            .read(&1u64.to_le_bytes())
            .read("a".as_bytes())
            .read(&[0u8; 7])
            .build();
        assert_eq!(
            "a".to_string(),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_8() {
        let mock = Builder::new()
            .read(&8u64.to_le_bytes())
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            "i'm gay.".to_string(),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_write_string_len_0() {
        let mock = Builder::new().write(&0u64.to_le_bytes()).build();
        NixWriter::new(mock, Version(0, 0))
            .write_string("")
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn test_write_string_len_1() {
        let mock = Builder::new()
            .write(&1u64.to_le_bytes())
            .write("a\0\0\0\0\0\0\0".as_bytes())
            .build();
        NixWriter::new(mock, Version(0, 0))
            .write_string("a")
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn test_write_string_len_8() {
        let mock = Builder::new()
            .write(&8u64.to_le_bytes())
            .write("i'm gay.".as_bytes())
            .build();
        NixWriter::new(mock, Version(0, 0))
            .write_string("i'm gay.")
            .await
            .unwrap();
    }

    // Long strings (infinite screaming).
    #[tokio::test]
    async fn test_read_string_len_1024() {
        let mock = Builder::new()
            .read(&1024u64.to_le_bytes())
            .read(&['a' as u8; 1024])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1024)),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_1025() {
        let mock = Builder::new()
            .read(&1025u64.to_le_bytes())
            .read(&['a' as u8; 1025])
            .read(&[0u8; 7])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1025)),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_2048() {
        let mock = Builder::new()
            .read(&2048u64.to_le_bytes())
            .read(&['a' as u8; 2048])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(2048)),
            NixReader::new(mock, Version(0, 0))
                .read_string()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_read_strings_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()).build();
        assert_eq!(
            Vec::<String>::new(),
            NixReader::new(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_1() {
        let mock = Builder::new()
            .read(&1u64.to_le_bytes())
            .read(&8u64.to_le_bytes())
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            vec!["i'm gay.".to_string()],
            NixReader::new(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_4() {
        let mock = Builder::new()
            .read(&4u64.to_le_bytes())
            .read(&22u64.to_le_bytes())
            .read("according to all known\0\0".as_bytes())
            .read(&16u64.to_le_bytes())
            .read("laws of aviation".as_bytes())
            .read(&25u64.to_le_bytes())
            .read("there's no way that a bee\0\0\0\0\0\0\0".as_bytes())
            .read(&21u64.to_le_bytes())
            .read("should be able to fly\0\0\0".as_bytes())
            .build();
        assert_eq!(
            vec![
                "according to all known".to_string(),
                "laws of aviation".to_string(),
                "there's no way that a bee".to_string(),
                "should be able to fly".to_string()
            ],
            NixReader::new(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_read_pathinfo_derived() {
        let mock = Builder::new()
            .read(&61u64.to_le_bytes()) // deriver
            .read(&pad_str::<64>(
                "/nix/store/xc1b35sn5lzqwpx23lzdfbhshbdbsdr1-sqlite-3.43.2.drv",
            ))
            .read(&51u64.to_le_bytes()) // nar_hash
            .read(&pad_str::<56>(
                "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=",
            ))
            .read(&2u64.to_le_bytes()) // references[]
            .read(&52u64.to_le_bytes()) // references[0]
            .read(&pad_str::<56>(
                "/nix/store/8xgb8phqmfn9h971q7dg369h647i1aa0-zlib-1.3",
            ))
             .read(&57u64.to_le_bytes()) // references[1]
             .read(&pad_str::<64>(
                 "/nix/store/qn3ggz5sf3hkjs2c797xf7nan3amdxmp-glibc-2.38-27",
             ))
             .read(&1700495600u64.to_le_bytes()) // registration_time
             .read(&1768960u64.to_le_bytes()) // nar_size
             .read(&0u64.to_le_bytes()) // ultimate
             .read(&1u64.to_le_bytes()) // signatures[]
             .read(&106u64.to_le_bytes()) // signatures[0]
             .read(&pad_str::<112>(
                 "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==",
             ))
             .read(&0u64.to_le_bytes()) // ca
            .build();
        assert_eq!(
            PathInfo {
                deriver: Some("/nix/store/xc1b35sn5lzqwpx23lzdfbhshbdbsdr1-sqlite-3.43.2.drv".into()),
                nar_hash: "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=".into(),
                references: vec![
                    "/nix/store/8xgb8phqmfn9h971q7dg369h647i1aa0-zlib-1.3".into(),
                    "/nix/store/qn3ggz5sf3hkjs2c797xf7nan3amdxmp-glibc-2.38-27".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 20, 15, 53, 20).unwrap(),
                nar_size: 1768960,
                ultimate: false,
                signatures: vec![
                    "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==".into(),
                ],
                ca: None,
            },
            NixReader::new(mock, Version(1, 35))
                .read_pathinfo()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_pathinfo_ca() {
        let mock = Builder::new()
            .read(&0u64.to_le_bytes()) // deriver
            .read(&51u64.to_le_bytes()) // nar_hash
            .read(&pad_str::<56>(
                "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=",
            ))
            .read(&5u64.to_le_bytes()) // references[]
            .read(&60u64.to_le_bytes()) // references[0]
            .read(&pad_str::<64>(
                "/nix/store/09wshq4g5mc2xjx24wmxlw018ly5mxgl-bash-5.2-p15.drv",
            ))
            .read(&58u64.to_le_bytes()) // references[1]
            .read(&pad_str::<64>(
                "/nix/store/74b93p6rw3xjrg0nds4dq2jpi66fapc1-curl-8.4.0.drv",
            ))
            .read(&54u64.to_le_bytes()) // references[2]
            .read(&pad_str::<56>(
                "/nix/store/g0gn91m56b267ncx05w93kihyqia39cm-builder.sh",
            ))
            .read(&60u64.to_le_bytes()) // references[3]
            .read(&pad_str::<64>(
                "/nix/store/mb9hk9cqwgrgl7gyipypn2h1wfz49h4s-stdenv-linux.drv",
            ))
            .read(&60u64.to_le_bytes()) // references[4]
            .read(&pad_str::<64>(
                "/nix/store/qbymsj2c80smzdqp0bx3z5minxri0ri3-mirrors-list.drv",
            ))
            .read(&1700854586u64.to_le_bytes()) // registration_time
            .read(&3008u64.to_le_bytes()) // nar_size
            .read(&0u64.to_le_bytes()) // ultimate
            .read(&0u64.to_le_bytes()) // signatures[]
            .read(&64u64.to_le_bytes()) // ca
            .read(&pad_str::<64>(
                "text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d",
            ))
            .build();
        assert_eq!(
            PathInfo {
                deriver: None,
                nar_hash: "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=".into(),
                references: vec![
                    "/nix/store/09wshq4g5mc2xjx24wmxlw018ly5mxgl-bash-5.2-p15.drv".into(),
                    "/nix/store/74b93p6rw3xjrg0nds4dq2jpi66fapc1-curl-8.4.0.drv".into(),
                    "/nix/store/g0gn91m56b267ncx05w93kihyqia39cm-builder.sh".into(),
                    "/nix/store/mb9hk9cqwgrgl7gyipypn2h1wfz49h4s-stdenv-linux.drv".into(),
                    "/nix/store/qbymsj2c80smzdqp0bx3z5minxri0ri3-mirrors-list.drv".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 24, 19, 36, 26).unwrap(),
                nar_size: 3008,
                ultimate: false,
                signatures: vec![],
                ca: Some("text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d".into()),
            },
            NixReader::new(mock, Version(1, 35))
                .read_pathinfo()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_write_pathinfo_derived() {
        let mock = Builder::new()
            .write(&61u64.to_le_bytes()) // deriver
            .write(&pad_str::<64>(
                "/nix/store/xc1b35sn5lzqwpx23lzdfbhshbdbsdr1-sqlite-3.43.2.drv",
            ))
            .write(&51u64.to_le_bytes()) // nar_hash
            .write(&pad_str::<56>(
                "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=",
            ))
            .write(&2u64.to_le_bytes()) // references[]
            .write(&52u64.to_le_bytes()) // references[0]
            .write(&pad_str::<56>(
                "/nix/store/8xgb8phqmfn9h971q7dg369h647i1aa0-zlib-1.3",
            ))
             .write(&57u64.to_le_bytes()) // references[1]
             .write(&pad_str::<64>(
                 "/nix/store/qn3ggz5sf3hkjs2c797xf7nan3amdxmp-glibc-2.38-27",
             ))
             .write(&1700495600u64.to_le_bytes()) // registration_time
             .write(&1768960u64.to_le_bytes()) // nar_size
             .write(&0u64.to_le_bytes()) // ultimate
             .write(&1u64.to_le_bytes()) // signatures[]
             .write(&106u64.to_le_bytes()) // signatures[0]
             .write(&pad_str::<112>(
                 "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==",
             ))
             .write(&0u64.to_le_bytes()) // ca
            .build();
        NixWriter::new(mock, Version(1, 35)).write_pathinfo(
            &PathInfo {
                deriver: Some("/nix/store/xc1b35sn5lzqwpx23lzdfbhshbdbsdr1-sqlite-3.43.2.drv".into()),
                nar_hash: "sha256-sUu8vqpIoy7ZpnQPcwvQasNqX2jJOSXeEwd1yFtTukU=".into(),
                references: vec![
                    "/nix/store/8xgb8phqmfn9h971q7dg369h647i1aa0-zlib-1.3".into(),
                    "/nix/store/qn3ggz5sf3hkjs2c797xf7nan3amdxmp-glibc-2.38-27".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 20, 15, 53, 20).unwrap(),
                nar_size: 1768960,
                ultimate: false,
                signatures: vec![
                    "cache.nixos.org-1:Efz+S0y30Eny+nbjeiS0vlUiEpmNbW+m1CiznlC5odPRpTfQUENj+AQcDsnEgvXmaTY9OqG0l5pMIBc6XAk6AQ==".into(),
                ],
                ca: None,
            })
                .await
                .unwrap();
    }
    #[tokio::test]
    async fn test_write_pathinfo_ca() {
        let mock = Builder::new()
            .write(&0u64.to_le_bytes()) // deriver
            .write(&51u64.to_le_bytes()) // nar_hash
            .write(&pad_str::<56>(
                "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=",
            ))
            .write(&5u64.to_le_bytes()) // references[]
            .write(&60u64.to_le_bytes()) // references[0]
            .write(&pad_str::<64>(
                "/nix/store/09wshq4g5mc2xjx24wmxlw018ly5mxgl-bash-5.2-p15.drv",
            ))
            .write(&58u64.to_le_bytes()) // references[1]
            .write(&pad_str::<64>(
                "/nix/store/74b93p6rw3xjrg0nds4dq2jpi66fapc1-curl-8.4.0.drv",
            ))
            .write(&54u64.to_le_bytes()) // references[2]
            .write(&pad_str::<56>(
                "/nix/store/g0gn91m56b267ncx05w93kihyqia39cm-builder.sh",
            ))
            .write(&60u64.to_le_bytes()) // references[3]
            .write(&pad_str::<64>(
                "/nix/store/mb9hk9cqwgrgl7gyipypn2h1wfz49h4s-stdenv-linux.drv",
            ))
            .write(&60u64.to_le_bytes()) // references[4]
            .write(&pad_str::<64>(
                "/nix/store/qbymsj2c80smzdqp0bx3z5minxri0ri3-mirrors-list.drv",
            ))
            .write(&1700854586u64.to_le_bytes()) // registration_time
            .write(&3008u64.to_le_bytes()) // nar_size
            .write(&0u64.to_le_bytes()) // ultimate
            .write(&0u64.to_le_bytes()) // signatures[]
            .write(&64u64.to_le_bytes()) // ca
            .write(&pad_str::<64>(
                "text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d",
            ))
            .build();
        NixWriter::new(mock, Version(1, 35))
            .write_pathinfo(&PathInfo {
                deriver: None,
                nar_hash: "sha256-1JmbR4NOsYNvgbJlqjp+4/bfm22IvhakiE1DXNfx78s=".into(),
                references: vec![
                    "/nix/store/09wshq4g5mc2xjx24wmxlw018ly5mxgl-bash-5.2-p15.drv".into(),
                    "/nix/store/74b93p6rw3xjrg0nds4dq2jpi66fapc1-curl-8.4.0.drv".into(),
                    "/nix/store/g0gn91m56b267ncx05w93kihyqia39cm-builder.sh".into(),
                    "/nix/store/mb9hk9cqwgrgl7gyipypn2h1wfz49h4s-stdenv-linux.drv".into(),
                    "/nix/store/qbymsj2c80smzdqp0bx3z5minxri0ri3-mirrors-list.drv".into(),
                ],
                registration_time: Utc.with_ymd_and_hms(2023, 11, 24, 19, 36, 26).unwrap(),
                nar_size: 3008,
                ultimate: false,
                signatures: vec![],
                ca: Some("text:sha256:0yjycizc8v9950dz9a69a7qlzcba9gl2gls8svi1g1i75xxf206d".into()),
            })
            .await
            .unwrap();
    }
}
