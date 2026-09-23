// SPDX-License-Identifier: GPL-2.0-only

//! Extracts production files from UBI/UBIFS and JFFS2 partition images.
use crate::model::{Result, crc32_register};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};

pub type Files = BTreeMap<String, Vec<u8>>;
const MAX_FILE: usize = 16 * 1024 * 1024;
fn bytes(d: &[u8], o: usize, n: usize) -> Result<&[u8]> {
    d.get(o..o.checked_add(n).ok_or("Length overflow")?)
        .ok_or("Image data is truncated".into())
}
fn le16(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(d, o, 2)?.try_into().unwrap()))
}
fn le32(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(d, o, 4)?.try_into().unwrap()))
}
fn le64(d: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(d, o, 8)?.try_into().unwrap()))
}
fn be32(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(bytes(d, o, 4)?.try_into().unwrap()))
}
fn be64(d: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(bytes(d, o, 8)?.try_into().unwrap()))
}

fn check_crc(d: &[u8], initial: u32, expected: u32) -> Result<()> {
    let actual = crc32_register(d, initial);
    if actual != expected {
        return Err(format!(
            "Stock image CRC mismatch: stored 0x{expected:08x}, calculated 0x{actual:08x}"
        ));
    }
    Ok(())
}
fn decompress(d: &[u8], size: usize, kind: u16) -> Result<Vec<u8>> {
    if size > MAX_FILE {
        return Err("File block is too large".into());
    }
    let out = match kind {
        0 => d.to_vec(),
        1 => {
            lzokay_native::decompress(&mut Cursor::new(d), Some(size)).map_err(|e| e.to_string())?
        }
        2 => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(d)
                .take((size + 1) as u64)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            out
        }
        4 => {
            // UBIFS stores raw DEFLATE while JFFS2 compression type 6 keeps the zlib wrapper.
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(d)
                .take((size + 1) as u64)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            out
        }
        _ => return Err(format!("Unsupported compression type {kind}")),
    };
    if out.len() != size {
        return Err("Decompressed length differs from the node header".into());
    }
    Ok(out)
}
fn name(d: &[u8]) -> Result<String> {
    let s = std::str::from_utf8(d).map_err(|e| e.to_string())?;
    if s.is_empty() || s.contains(['/', '\0']) || s == "." || s == ".." {
        return Err("Invalid directory entry name".into());
    }
    Ok(s.into())
}

/// Reconstructs file paths from directory entries and inode payloads.
fn paths(
    dents: &BTreeMap<(u64, String), u64>,
    sizes: &BTreeMap<u64, usize>,
    read: impl Fn(u64, usize) -> Result<Vec<u8>>,
) -> Result<Files> {
    let mut result = Files::new();
    let mut todo = vec![(1u64, String::new())];
    let mut seen = BTreeSet::new();
    while let Some((parent, prefix)) = todo.pop() {
        if !seen.insert(parent) {
            return Err("Filesystem tree contains a cycle or duplicate reference".into());
        }
        for ((p, n), ino) in dents {
            if *p != parent || *ino == 0 {
                continue;
            }
            let path = if prefix.is_empty() {
                n.clone()
            } else {
                format!("{prefix}/{n}")
            };
            if let Some(&size) = sizes.get(ino) {
                if size > MAX_FILE {
                    return Err(format!("{path} is too large"));
                }
                result.insert(path, read(*ino, size)?);
            } else {
                todo.push((*ino, path));
            }
        }
    }
    Ok(result)
}

pub fn extract(d: &[u8]) -> Result<(Files, bool)> {
    if d.starts_with(b"UBI#") {
        ubi(d)
    } else if d.starts_with(&[0x85, 0x19]) {
        jffs2(d)
    } else {
        Err("Unsupported factory image format".into())
    }
}

fn ubi(d: &[u8]) -> Result<(Files, bool)> {
    let peb = (16..=20)
        .map(|n| 1usize << n)
        .find(|&s| d.len().is_multiple_of(s) && d.get(s..s + 4) == Some(b"UBI#"))
        .ok_or("Unable to determine the UBI eraseblock size")?;
    let mut blocks: BTreeMap<(u32, u32), (u64, &[u8])> = BTreeMap::new();
    for p in d.chunks_exact(peb) {
        if p.iter().all(|b| *b == 255) {
            continue;
        }
        if !p.starts_with(b"UBI#") {
            return Err("Invalid UBI erase counter header".into());
        }
        check_crc(bytes(p, 0, 60)?, u32::MAX, be32(p, 60)?)?;
        let vo = be32(p, 16)? as usize;
        let data = be32(p, 20)? as usize;
        let vid = bytes(p, vo, 64)?;
        if vid.iter().all(|b| *b == 255) {
            continue;
        }
        if !vid.starts_with(b"UBI!") {
            return Err("Invalid UBI VID header".into());
        }
        check_crc(&vid[..60], u32::MAX, be32(vid, 60)?)?;
        let key = (be32(vid, 8)?, be32(vid, 12)?);
        let seq = be64(vid, 40)?;
        let payload = bytes(
            p,
            data,
            peb.checked_sub(data).ok_or("Invalid UBI data offset")?,
        )?;
        // A copy-flagged PEB becomes eligible after its data CRC confirms the relocation.
        if vid[6] != 0 || vid[5] == 2 {
            let valid = check_crc(
                bytes(payload, 0, be32(vid, 20)? as usize)?,
                u32::MAX,
                be32(vid, 32)?,
            );
            if valid.is_err() && vid[6] != 0 {
                continue;
            }
            valid?;
        }
        if blocks.get(&key).is_none_or(|(old, _)| seq > *old) {
            blocks.insert(key, (seq, payload));
        }
    }
    let table = blocks
        .get(&(0x7fffefff, 0))
        .or_else(|| blocks.get(&(0x7fffefff, 1)))
        .ok_or("UBI volume table is missing")?
        .1;
    let mut id = None;
    for (i, rec) in table.chunks_exact(172).take(128).enumerate() {
        let len = u16::from_be_bytes([rec[14], rec[15]]) as usize;
        if len > 127 {
            return Err("Invalid UBI volume name length".into());
        }
        if bytes(rec, 16, len)? == b"fhdata" {
            check_crc(&rec[..168], u32::MAX, be32(rec, 168)?)?;
            if rec[13] != 0 {
                return Err("fhdata volume update is incomplete".into());
            }
            id = Some(i as u32);
            break;
        }
    }
    let id = id.ok_or("UBI image has no fhdata volume")?;
    let lebs: BTreeMap<u32, &[u8]> = blocks
        .into_iter()
        .filter_map(|((v, l), (_, p))| (v == id).then_some((l, p)))
        .collect();
    ubifs(&lebs)
}
fn node(d: &[u8], offset: usize) -> Result<&[u8]> {
    let h = bytes(d, offset, 24)?;
    if le32(h, 0)? != 0x06101831 {
        return Err("Invalid UBIFS node header".into());
    }
    let n = bytes(d, offset, le32(h, 16)? as usize)?;
    if n.len() < 24 {
        return Err("Invalid UBIFS node length".into());
    }
    check_crc(&n[8..], u32::MAX, le32(n, 4)?)?;
    Ok(n)
}
fn ubifs(lebs: &BTreeMap<u32, &[u8]>) -> Result<(Files, bool)> {
    let sb = node(lebs.get(&0).ok_or("UBIFS superblock is missing")?, 0)?;
    if sb[20] != 6 || *sb.get(27).ok_or("UBIFS superblock is truncated")? != 0 {
        return Err("Unsupported UBIFS key format".into());
    }
    // The newest valid master node identifies the committed index root.
    let mut master = None;
    for l in [1, 2] {
        if let Some(d) = lebs.get(&l) {
            for off in (0..d.len().saturating_sub(24)).step_by(8) {
                if d.get(off..off + 4) != Some(&[0x31, 0x18, 0x10, 0x06]) {
                    continue;
                }
                if let Ok(n) = node(d, off)
                    && n[20] == 7
                    && master.is_none_or(|old: &[u8]| {
                        le64(n, 8).unwrap_or(0) > le64(old, 8).unwrap_or(0)
                    })
                {
                    master = Some(n);
                }
            }
        }
    }
    let mst = master.ok_or("UBIFS master node is missing")?;
    // The dirty flag is surfaced because the master node still points to the last commit.
    let uncommitted = le32(mst, 40)? & 1 != 0;
    let mut todo = vec![(
        le32(mst, 48)?,
        le32(mst, 52)? as usize,
        le32(mst, 56)? as usize,
    )];
    let mut seen = BTreeSet::new();
    let mut dents = BTreeMap::new();
    let mut sizes = BTreeMap::new();
    let mut chunks: BTreeMap<(u64, u32), &[u8]> = BTreeMap::new();
    while let Some((l, o, len)) = todo.pop() {
        if !seen.insert((l, o)) {
            return Err("UBIFS index contains a duplicate reference".into());
        }
        let n = node(
            lebs.get(&l).ok_or("UBIFS index references a missing LEB")?,
            o,
        )?;
        if n.len() != len {
            return Err("UBIFS index node length mismatch".into());
        }
        match n[20] {
            9 => {
                let count = le16(n, 24)? as usize;
                if n.len() != 28 + count * 20 {
                    return Err("Unsupported UBIFS index branch format".into());
                }
                for b in n[28..].chunks_exact(20) {
                    todo.push((le32(b, 0)?, le32(b, 4)? as usize, le32(b, 8)? as usize));
                }
            }
            0 => {
                if le32(n, 104)? & 0xf000 == 0x8000 {
                    sizes.insert(
                        le32(n, 24)? as u64,
                        usize::try_from(le64(n, 48)?).map_err(|e| e.to_string())?,
                    );
                }
            }
            2 => {
                dents.insert(
                    (
                        le32(n, 24)? as u64,
                        name(bytes(n, 56, le16(n, 50)? as usize)?)?,
                    ),
                    le64(n, 40)?,
                );
            }
            1 => {
                bytes(n, 0, 48)?;
                if le32(n, 40)? > 4096 {
                    return Err("UBIFS data block exceeds 4096 bytes".into());
                }
                chunks.insert((le32(n, 24)? as u64, le32(n, 28)? & 0x1fffffff), n);
            }
            3 => {}
            k => return Err(format!("UBIFS index contains unknown node type {k}")),
        }
    }
    let files = paths(&dents, &sizes, |ino, size| {
        let mut out = vec![0; size];
        for ((i, block), n) in &chunks {
            if *i != ino {
                continue;
            }
            let compression = le16(n, 44)?;
            let decoded = decompress(
                &n[48..],
                le32(n, 40)? as usize,
                if compression == 2 { 4 } else { compression },
            )?;
            let start = (*block as usize)
                .checked_mul(4096)
                .ok_or("Data block offset overflow")?;
            if start >= size {
                return Err("UBIFS data block exceeds the file size".into());
            }
            let count = decoded.len().min(size - start);
            out[start..start + count].copy_from_slice(&decoded[..count]);
        }
        Ok(out)
    })?;
    Ok((files, uncommitted))
}

fn jffs2(d: &[u8]) -> Result<(Files, bool)> {
    let mut dent_versions: BTreeMap<(u64, String), (u32, u64)> = BTreeMap::new();
    let mut inode_nodes: BTreeMap<u64, Vec<&[u8]>> = BTreeMap::new();
    let mut damaged = false;
    let mut off = 0;
    while off + 12 <= d.len() {
        if le16(d, off)? != 0x1985 {
            off += 4;
            continue;
        }
        let kind = le16(d, off + 2)?;
        let len = le32(d, off + 4)? as usize;
        if len < 12 {
            return Err("Invalid JFFS2 node length".into());
        }
        let n = bytes(d, off, len)?;
        off = off
            .checked_add((len + 3) & !3)
            .ok_or("JFFS2 offset overflow")?;
        // ACCURATE is part of the header CRC; GC clears the bit to mark obsolete nodes.
        if kind & 0x2000 == 0 {
            continue;
        }
        // CRC-valid copies can reconstruct an inode when an older copy is damaged.
        if check_crc(&n[..8], 0, le32(n, 8)?).is_err() {
            damaged = true;
            continue;
        }
        match kind {
            0xe001 => {
                if check_crc(bytes(n, 0, 32)?, 0, le32(n, 32)?).is_err() {
                    damaged = true;
                    continue;
                }
                let text = bytes(
                    n,
                    40,
                    *n.get(28).ok_or("JFFS2 dirent is truncated")? as usize,
                )?;
                if check_crc(text, 0, le32(n, 36)?).is_err() {
                    damaged = true;
                    continue;
                }
                let key = (le32(n, 12)? as u64, name(text)?);
                let ver = le32(n, 16)?;
                let ino = le32(n, 20)? as u64;
                if dent_versions.get(&key).is_none_or(|(v, _)| ver > *v) {
                    dent_versions.insert(key, (ver, ino));
                }
            }
            0xe002 => {
                if check_crc(bytes(n, 0, 60)?, 0, le32(n, 64)?).is_err()
                    || check_crc(bytes(n, 68, le32(n, 48)? as usize)?, 0, le32(n, 60)?).is_err()
                {
                    damaged = true;
                    continue;
                }
                inode_nodes.entry(le32(n, 12)? as u64).or_default().push(n);
            }
            _ => {}
        }
    }
    let dents = dent_versions
        .into_iter()
        .map(|(k, (_, i))| (k, i))
        .collect();
    let mut sizes = BTreeMap::new();
    for (ino, nodes) in &mut inode_nodes {
        nodes.sort_by_key(|n| le32(n, 16).unwrap_or(0));
        let last = nodes.last().unwrap();
        if le32(last, 20)? & 0xf000 == 0x8000 {
            sizes.insert(*ino, le32(last, 28)? as usize);
        }
    }
    let files = paths(&dents, &sizes, |ino, size| {
        let mut out = Vec::new();
        for n in inode_nodes.get(&ino).ok_or("JFFS2 inode is missing")? {
            let file_size = le32(n, 28)? as usize;
            if file_size > MAX_FILE {
                return Err("JFFS2 file is too large".into());
            }
            out.resize(file_size, 0);
            let offset = le32(n, 44)? as usize;
            let expected = le32(n, 52)? as usize;
            let input = bytes(n, 68, le32(n, 48)? as usize)?;
            let data = match n[56] {
                0 => decompress(input, expected, 0)?,
                1 => vec![0; expected],
                2 => rtime(input, expected)?,
                6 => decompress(input, expected, 2)?,
                7 => decompress(input, expected, 1)?,
                k => return Err(format!("Unsupported JFFS2 compression type {k}")),
            };
            let end = offset
                .checked_add(data.len())
                .ok_or("JFFS2 data offset overflow")?;
            if end > out.len() {
                return Err("JFFS2 data exceeds the inode size".into());
            }
            out[offset..end].copy_from_slice(&data);
        }
        if out.len() != size {
            return Err("JFFS2 file length mismatch".into());
        }
        Ok(out)
    })?;
    Ok((files, damaged))
}
fn rtime(d: &[u8], size: usize) -> Result<Vec<u8>> {
    if size > MAX_FILE {
        return Err("RTIME output is too large".into());
    }
    let mut out = Vec::with_capacity(size);
    let mut positions = [0usize; 256];
    for p in d.chunks_exact(2) {
        if out.len() >= size {
            break;
        }
        let b = p[0];
        out.push(b);
        let start = positions[b as usize];
        positions[b as usize] = out.len();
        for j in 0..p[1] as usize {
            if out.len() >= size {
                return Err("RTIME output exceeds the declared length".into());
            }
            out.push(*out.get(start + j).ok_or("Invalid RTIME back-reference")?);
        }
    }
    if out.len() != size {
        return Err("RTIME output is shorter than the declared length".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn finish_jffs2_header(data: &mut [u8], kind: u16) {
        data[0..2].copy_from_slice(&0x1985u16.to_le_bytes());
        data[2..4].copy_from_slice(&kind.to_le_bytes());
        put_u32(data, 4, data.len() as u32);
        put_u32(data, 8, crc32_register(&data[..8], 0));
    }

    fn jffs2_dirent(name: &[u8], ino: u32) -> Vec<u8> {
        let mut node = vec![0; 40 + name.len()];
        put_u32(&mut node, 12, 1);
        put_u32(&mut node, 16, 1);
        put_u32(&mut node, 20, ino);
        node[28] = name.len() as u8;
        node[29] = 8;
        node[40..].copy_from_slice(name);
        finish_jffs2_header(&mut node, 0xe001);
        let node_crc = crc32_register(&node[..32], 0);
        put_u32(&mut node, 32, node_crc);
        put_u32(&mut node, 36, crc32_register(name, 0));
        node
    }

    fn jffs2_inode(data: &[u8], corrupt: bool) -> Vec<u8> {
        let mut node = vec![0; 68 + data.len()];
        put_u32(&mut node, 12, 8);
        put_u32(&mut node, 16, 3);
        put_u32(&mut node, 20, 0x81a4);
        put_u32(&mut node, 28, data.len() as u32);
        put_u32(&mut node, 48, data.len() as u32);
        put_u32(&mut node, 52, data.len() as u32);
        node[68..].copy_from_slice(data);
        finish_jffs2_header(&mut node, 0xe002);
        put_u32(&mut node, 60, crc32_register(data, 0));
        let node_crc = crc32_register(&node[..60], 0);
        put_u32(&mut node, 64, node_crc);
        if corrupt {
            node[68] ^= 1;
        }
        node
    }

    fn append_jffs2_node(image: &mut Vec<u8>, node: &[u8]) {
        image.extend_from_slice(node);
        image.resize((image.len() + 3) & !3, 0xff);
    }

    fn make_node(kind: u8, mut data: Vec<u8>) -> Vec<u8> {
        data[..4].copy_from_slice(&0x06101831u32.to_le_bytes());
        let len = data.len() as u32;
        data[16..20].copy_from_slice(&len.to_le_bytes());
        data[20] = kind;
        let checksum = crc32_register(&data[8..], u32::MAX);
        data[4..8].copy_from_slice(&checksum.to_le_bytes());
        data
    }

    #[test]
    fn dirty_master_uses_committed_index() {
        let sb = make_node(6, vec![0; 64]);
        let index = make_node(9, vec![0; 28]);
        let mut raw_master = vec![0; 64];
        raw_master[40..44].copy_from_slice(&1u32.to_le_bytes());
        raw_master[48..52].copy_from_slice(&3u32.to_le_bytes());
        raw_master[56..60].copy_from_slice(&28u32.to_le_bytes());
        let master = make_node(7, raw_master);
        let lebs = BTreeMap::from([
            (0, sb.as_slice()),
            (1, master.as_slice()),
            (3, index.as_slice()),
        ]);
        let (files, warning) = ubifs(&lebs).unwrap();
        assert!(warning);
        assert!(files.is_empty());

        let mut corrupt = index.clone();
        corrupt[24] ^= 1;
        let lebs = BTreeMap::from([
            (0, sb.as_slice()),
            (1, master.as_slice()),
            (3, corrupt.as_slice()),
        ]);
        assert!(ubifs(&lebs).is_err());
    }

    #[test]
    fn jffs2_skips_corrupt_inode_copy() {
        let mut image = Vec::new();
        append_jffs2_node(&mut image, &jffs2_dirent(b"tr.conf", 8));
        append_jffs2_node(&mut image, &jffs2_inode(b"hello", true));
        append_jffs2_node(&mut image, &jffs2_inode(b"hello", false));

        let (files, damaged) = jffs2(&image).unwrap();
        assert!(damaged);
        assert_eq!(files["tr.conf"], b"hello");
    }
}
