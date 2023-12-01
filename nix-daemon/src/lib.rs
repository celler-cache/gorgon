// SPDX-FileCopyrightText: 2023 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use async_stream::try_stream;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_stream::Stream;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u32, u32);

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

pub struct NixReader<R: AsyncReadExt + Unpin>(R, Version);

impl<R: AsyncReadExt + Unpin> NixReader<R> {
    /// Read a u64 from the stream (little endian).
    pub async fn read_u64(&mut self) -> Result<u64> {
        Ok(self.0.read_u64_le().await?)
    }

    /// Read a boolean from the stream, encoded as u64 (>0 is true).
    pub async fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_u64().await? > 0)
    }

    /// Reads the exact number of bytes needed to fill up `buf`.
    pub async fn read_exact<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a [u8]> {
        self.0.read_exact(buf).await?;
        Ok(buf)
    }

    /// Reads a string from the stream. Strings are prefixed with a u64 length, but the
    /// data is padded to the next 8-byte boundary, eg. a 1-byte string becomes 16 bytes
    /// on the wire: 8 for the length, 1 for the data, then 7 bytes of discarded 0x00s.
    pub async fn read_string(&mut self) -> Result<String> {
        let len = self.read_u64().await? as usize;
        let padded_len = len + if len % 8 > 0 { 8 - (len % 8) } else { 0 };
        if padded_len <= 1024 {
            let mut buf = [0u8; 1024];
            self.read_exact(&mut buf[..padded_len]).await?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            let mut buf = vec![0u8; padded_len];
            self.read_exact(&mut buf[..padded_len]).await?;
            Ok(String::from_utf8_lossy(&buf[..len]).to_string())
        }
    }

    /// Reads a list (or set) of strings from the stream - a u64 count, followed by that
    /// many strings using the normal `read_string()` encoding.
    pub fn read_strings(&mut self) -> impl Stream<Item = Result<String>> + '_ {
        try_stream! {
            let count = self.read_u64().await? as usize;
            for _ in 0..count {
                yield self.read_string().await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;
    use tokio_test::io::Builder;

    // Integers.
    #[tokio::test]
    async fn test_read_u64() {
        let mock = Builder::new()
            .read(&1234567890u64.to_le_bytes()[..])
            .build();
        assert_eq!(
            1234567890u64,
            NixReader(mock, Version(0, 0)).read_u64().await.unwrap()
        );
    }

    // Booleans.
    #[tokio::test]
    async fn test_read_bool_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()[..]).build();
        assert_eq!(
            false,
            NixReader(mock, Version(0, 0)).read_bool().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_bool_1() {
        let mock = Builder::new().read(&1u64.to_le_bytes()[..]).build();
        assert_eq!(
            true,
            NixReader(mock, Version(0, 0)).read_bool().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_bool_2() {
        let mock = Builder::new().read(&2u64.to_le_bytes()[..]).build();
        assert_eq!(
            true,
            NixReader(mock, Version(0, 0)).read_bool().await.unwrap()
        );
    }

    // Short strings.
    #[tokio::test]
    async fn test_read_string_len_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()[..]).build();
        assert_eq!(
            "".to_string(),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_1() {
        let mock = Builder::new()
            .read(&1u64.to_le_bytes()[..])
            .read("a".as_bytes())
            .read(&[0u8; 7])
            .build();
        assert_eq!(
            "a".to_string(),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_8() {
        let mock = Builder::new()
            .read(&8u64.to_le_bytes()[..])
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            "i'm gay.".to_string(),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }

    // Long strings (infinite screaming).
    #[tokio::test]
    async fn test_read_string_len_1024() {
        let mock = Builder::new()
            .read(&1024u64.to_le_bytes()[..])
            .read(&['a' as u8; 1024])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1024)),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_1025() {
        let mock = Builder::new()
            .read(&1025u64.to_le_bytes()[..])
            .read(&['a' as u8; 1025])
            .read(&[0u8; 7])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(1025)),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_string_len_2048() {
        let mock = Builder::new()
            .read(&2048u64.to_le_bytes()[..])
            .read(&['a' as u8; 2048])
            .build();
        assert_eq!(
            String::from_iter(std::iter::repeat('a').take(2048)),
            NixReader(mock, Version(0, 0)).read_string().await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_read_strings_0() {
        let mock = Builder::new().read(&0u64.to_le_bytes()[..]).build();
        assert_eq!(
            Vec::<String>::new(),
            NixReader(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_1() {
        let mock = Builder::new()
            .read(&1u64.to_le_bytes()[..])
            .read(&8u64.to_le_bytes()[..])
            .read("i'm gay.".as_bytes())
            .build();
        assert_eq!(
            vec!["i'm gay.".to_string()],
            NixReader(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn test_read_strings_4() {
        let mock = Builder::new()
            .read(&4u64.to_le_bytes()[..])
            .read(&22u64.to_le_bytes()[..])
            .read("according to all known\0\0".as_bytes())
            .read(&16u64.to_le_bytes()[..])
            .read("laws of aviation".as_bytes())
            .read(&25u64.to_le_bytes()[..])
            .read("there's no way that a bee\0\0\0\0\0\0\0".as_bytes())
            .read(&21u64.to_le_bytes()[..])
            .read("should be able to fly\0\0\0".as_bytes())
            .build();
        assert_eq!(
            vec![
                "according to all known".to_string(),
                "laws of aviation".to_string(),
                "there's no way that a bee".to_string(),
                "should be able to fly".to_string()
            ],
            NixReader(mock, Version(0, 0))
                .read_strings()
                .collect::<Result<Vec<_>>>()
                .await
                .unwrap()
        );
    }
}
