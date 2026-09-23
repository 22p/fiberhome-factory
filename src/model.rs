// SPDX-License-Identifier: GPL-2.0-only

//! Builds the fixed 1 MiB factory layout from FiberHome production data.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const FACTORY_SIZE: usize = 0x100000;

// These offsets are the ABI consumed by an7581-fiberhome-ubi-parts.dtsi.
const PON_OFFSET: usize = 0x0000;
const PON_SIZE: usize = 0x0800;
const WIFI_OFFSET: usize = 0x1000;
const WIFI_SIZE: usize = 0x1000;
const BASE_MAC_OFFSET: usize = 0x2000;
const BASE_MAC_SIZE: usize = 0x0006;
const PON_SN_OFFSET: usize = 0x2010;
const PON_SN_SIZE: usize = 0x0008;
const DEVICE_SN_OFFSET: usize = 0x2020;
const DEVICE_SN_SIZE: usize = 0x0020;

const BDBOB_MAGIC: &[u8] = b"fiberhomebdbob";
const BDBOB_DATA_SIZE_OFFSET: usize = 0x70;
const BDBOB_DATA_CRC_OFFSET: usize = 0x74;
const BDBOB_HEADER_SIZE_OFFSET: usize = 0x78;
const BDBOB_HEADER_CRC_OFFSET: usize = 0x7c;
const BDBOB_HEADER_SIZE: usize = 0x80;
const PON_CAL_MAGIC: &[u8; 8] = b"APONCAL\0";
const PON_CAL_VERSION: u16 = 1;
const PON_CAL_HEADER_SIZE: usize = 0x80;
const PON_CAL_FORMAT_NATIVE: u32 = 1;
const PON_CAL_HEADER_CRC_OFFSET: usize = 0x7c;
const MT7916_DEVICE_ID: u16 = 0x7916;

pub type Result<T> = std::result::Result<T, String>;

fn read_file(path: &Path, description: &str) -> Result<Vec<u8>> {
    fs::read(path)
        .map_err(|error| format!("Unable to read {description} {}: {error}", path.display()))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("Data is truncated at offset 0x{offset:x}"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// Returns the raw reflected CRC-32 register after processing `data`.
pub(crate) fn crc32_register(data: &[u8], mut crc: u32) -> u32 {
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

// BDBOB complements a CRC register initialized to zero.
fn fiberhome_crc32(data: &[u8]) -> u32 {
    crc32_register(data, 0) ^ u32::MAX
}

fn calibration_crc32(data: &[u8]) -> u32 {
    crc32_register(data, u32::MAX) ^ u32::MAX
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PonCalibrationChip {
    Gn28l95 = 1,
    Ux3363 = 2,
}

impl PonCalibrationChip {
    fn from_id(id: u32) -> Result<Self> {
        match id {
            1 => Ok(Self::Gn28l95),
            2 => Ok(Self::Ux3363),
            _ => Err(format!("Unknown PON frontend calibration chip {id}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Gn28l95 => "GN28L95",
            Self::Ux3363 => "UX3363",
        }
    }
}

#[derive(Clone, Copy)]
struct PonCalibrationInfo {
    chip: PonCalibrationChip,
    payload_length: usize,
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| format!("Data is truncated at offset 0x{offset:x}"))?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn put_u16_le(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32_le(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// The 128-byte APONCAL header keeps the native payload at offset 0x80.
fn encode_pon_calibration(chip: PonCalibrationChip, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > PON_SIZE - PON_CAL_HEADER_SIZE {
        return Err(format!(
            "{} calibration data exceeds 1920 bytes",
            chip.name()
        ));
    }
    let mut output = vec![0xff; PON_SIZE];
    output[..PON_CAL_HEADER_SIZE].fill(0);
    output[..PON_CAL_MAGIC.len()].copy_from_slice(PON_CAL_MAGIC);
    put_u16_le(&mut output, 0x08, PON_CAL_VERSION);
    put_u16_le(&mut output, 0x0a, PON_CAL_HEADER_SIZE as u16);
    put_u32_le(&mut output, 0x0c, chip as u32);
    put_u32_le(&mut output, 0x10, PON_CAL_FORMAT_NATIVE);
    put_u32_le(&mut output, 0x14, payload.len() as u32);
    put_u32_le(&mut output, 0x18, calibration_crc32(payload));
    output[PON_CAL_HEADER_SIZE..PON_CAL_HEADER_SIZE + payload.len()].copy_from_slice(payload);
    let header_crc = calibration_crc32(&output[..PON_CAL_HEADER_CRC_OFFSET]);
    put_u32_le(&mut output, PON_CAL_HEADER_CRC_OFFSET, header_crc);
    Ok(output)
}

fn parse_pon_calibration(path: &Path, data: &[u8]) -> Result<PonCalibrationInfo> {
    if data.len() != PON_SIZE || !data.starts_with(PON_CAL_MAGIC) {
        return Err(format!(
            "Invalid PON calibration container {}",
            path.display()
        ));
    }
    if read_u16_le(data, 0x08)? != PON_CAL_VERSION
        || read_u16_le(data, 0x0a)? as usize != PON_CAL_HEADER_SIZE
        || read_u32_le(data, 0x10)? != PON_CAL_FORMAT_NATIVE
    {
        return Err(format!(
            "Unsupported PON calibration format {}",
            path.display()
        ));
    }
    if calibration_crc32(&data[..PON_CAL_HEADER_CRC_OFFSET]) != read_u32_le(data, 0x7c)? {
        return Err(format!(
            "PON calibration header CRC mismatch in {}",
            path.display()
        ));
    }
    let chip = PonCalibrationChip::from_id(read_u32_le(data, 0x0c)?)?;
    let payload_length = read_u32_le(data, 0x14)? as usize;
    let payload = data
        .get(PON_CAL_HEADER_SIZE..PON_CAL_HEADER_SIZE + payload_length)
        .ok_or_else(|| {
            format!(
                "Invalid PON calibration payload length in {}",
                path.display()
            )
        })?;
    if calibration_crc32(payload) != read_u32_le(data, 0x18)? {
        return Err(format!(
            "PON calibration payload CRC mismatch in {}",
            path.display()
        ));
    }
    Ok(PonCalibrationInfo {
        chip,
        payload_length,
    })
}

fn validate_bdbob(path: &Path, data: &[u8]) -> Result<()> {
    if data.len() != PON_SIZE {
        return Err(format!(
            "PON BDBOB {} is {} bytes; expected {}",
            path.display(),
            data.len(),
            PON_SIZE
        ));
    }
    if !data.starts_with(BDBOB_MAGIC) {
        return Err(format!(
            "PON BDBOB {} has no fiberhomebdbob header",
            path.display()
        ));
    }

    let data_size = read_u32_le(data, BDBOB_DATA_SIZE_OFFSET)? as usize;
    let data_crc = read_u32_le(data, BDBOB_DATA_CRC_OFFSET)?;
    let header_size = read_u32_le(data, BDBOB_HEADER_SIZE_OFFSET)? as usize;
    let header_crc = read_u32_le(data, BDBOB_HEADER_CRC_OFFSET)?;

    if header_size != BDBOB_HEADER_SIZE || header_size + data_size != data.len() {
        return Err(format!(
            "Invalid PON BDBOB length fields in {}",
            path.display()
        ));
    }
    if fiberhome_crc32(&data[..BDBOB_HEADER_CRC_OFFSET]) != header_crc {
        return Err(format!(
            "PON BDBOB header CRC mismatch in {}",
            path.display()
        ));
    }
    if fiberhome_crc32(&data[header_size..header_size + data_size]) != data_crc {
        return Err(format!(
            "PON BDBOB payload CRC mismatch in {}",
            path.display()
        ));
    }

    Ok(())
}

fn convert_bdbob(path: &Path, data: &[u8]) -> Result<Vec<u8>> {
    validate_bdbob(path, data)?;
    let chip = match data[0x30] {
        0 | 0x28 => PonCalibrationChip::Gn28l95,
        0x2e => PonCalibrationChip::Ux3363,
        id => {
            return Err(format!(
                "Unsupported PON BDBOB chip 0x{id:02x} in {}",
                path.display(),
            ));
        }
    };
    encode_pon_calibration(chip, &data[BDBOB_HEADER_SIZE..])
}

fn normalize_pon_calibration(path: &Path, data: &[u8]) -> Result<Vec<u8>> {
    if data.starts_with(PON_CAL_MAGIC) {
        parse_pon_calibration(path, data)?;
        Ok(data.to_vec())
    } else {
        convert_bdbob(path, data)
    }
}

// factory_conf stores each production value in a UCI key section.
fn parse_factory_conf(path: &Path, data: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(data)
        .map_err(|error| format!("Invalid text encoding in {}: {error}", path.display()))?;
    let mut fields = BTreeMap::new();
    let mut current_key: Option<String> = None;

    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() == 3 && words[0] == "config" && words[1] == "key" {
            current_key = unquote(words[2]).map(str::to_owned);
            continue;
        }
        if words.len() >= 3
            && words[0] == "option"
            && words[1] == "value"
            && let Some(key) = current_key.as_ref()
        {
            let value_text = line
                .split_once("value")
                .map(|(_, value)| value.trim())
                .unwrap_or_default();
            if let Some(value) = unquote(value_text) {
                fields.insert(key.clone(), value.to_owned());
            }
        }
    }

    Ok(fields)
}

fn unquote(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[bytes.len() - 1] == bytes[0] {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

fn required_field<'a>(
    path: &Path,
    fields: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{} is missing {name}", path.display()))
}

fn decode_hex(path: &Path, field: &str, value: &str, expected: usize) -> Result<Vec<u8>> {
    let compact: String = value
        .chars()
        .filter(|character| *character != ':' && *character != '-')
        .collect();
    if !compact.is_ascii() || compact.len() != expected * 2 {
        return Err(format!("Invalid {field} length in {}", path.display()));
    }

    let mut result = Vec::with_capacity(expected);
    for index in (0..compact.len()).step_by(2) {
        let byte = u8::from_str_radix(&compact[index..index + 2], 16)
            .map_err(|_| format!("Invalid hexadecimal {field} in {}", path.display()))?;
        result.push(byte);
    }
    Ok(result)
}

fn parse_mac(path: &Path, field: &str, value: &str) -> Result<Vec<u8>> {
    let mac = decode_hex(path, field, value, BASE_MAC_SIZE)?;
    if mac.iter().all(|byte| *byte == 0) || mac.iter().all(|byte| *byte == 0xff) || mac[0] & 1 != 0
    {
        return Err(format!("Invalid {field} in {}", path.display()));
    }
    Ok(mac)
}

fn parse_pon_serial(path: &Path, value: &str) -> Result<Vec<u8>> {
    let serial = decode_hex(path, "GponSN", value, PON_SN_SIZE)?;
    if serial.iter().all(|byte| *byte == 0) || serial.iter().all(|byte| *byte == 0xff) {
        return Err(format!("Invalid GponSN in {}", path.display()));
    }
    Ok(serial)
}

fn parse_device_serial(path: &Path, value: &str) -> Result<Vec<u8>> {
    let serial = value.as_bytes();
    if serial.is_empty()
        || serial.len() > DEVICE_SN_SIZE
        || !serial.iter().all(|byte| (0x20..=0x7e).contains(byte))
    {
        return Err(format!("Invalid SerialNumber in {}", path.display()));
    }

    let mut fixed = vec![0; DEVICE_SN_SIZE];
    fixed[..serial.len()].copy_from_slice(serial);
    Ok(fixed)
}

fn extract_wifi_eeprom(path: &Path, container: &[u8]) -> Result<Vec<u8>> {
    let eeprom = container.get(..WIFI_SIZE).ok_or_else(|| {
        format!(
            "MT7916 EEPROM {} is shorter than 4096 bytes",
            path.display()
        )
    })?;
    let device_id = u16::from_le_bytes([eeprom[0], eeprom[1]]);
    if device_id != MT7916_DEVICE_ID {
        return Err(format!("Invalid MT7916 device ID in {}", path.display()));
    }
    Ok(eeprom.to_vec())
}

fn place(image: &mut [u8], offset: usize, data: &[u8], name: &str) -> Result<()> {
    let destination = image
        .get_mut(offset..offset + data.len())
        .ok_or_else(|| format!("{name} exceeds the factory image boundary"))?;
    destination.copy_from_slice(data);
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Part {
    Pon,
    Wifi,
}

impl Part {
    pub fn range(self) -> std::ops::Range<usize> {
        match self {
            Self::Pon => PON_OFFSET..PON_OFFSET + PON_SIZE,
            Self::Wifi => WIFI_OFFSET..WIFI_OFFSET + WIFI_SIZE,
        }
    }
}

#[derive(Clone, Copy)]
pub enum ImportWarning {
    DirtyUbifs,
    DamagedJffs2,
}

pub enum PartStatus {
    Empty,
    Pon {
        chip: PonCalibrationChip,
        payload_length: usize,
    },
    Wifi {
        length: usize,
    },
    Invalid,
}

fn inspect_part(part: Part, data: &[u8]) -> Result<PartStatus> {
    match part {
        Part::Pon => {
            let info = parse_pon_calibration(Path::new("PON calibration"), data)?;
            Ok(PartStatus::Pon {
                chip: info.chip,
                payload_length: info.payload_length,
            })
        }
        Part::Wifi => {
            if data.len() != WIFI_SIZE {
                return Err("Wi-Fi EEPROM must be exactly 4096 bytes".into());
            }
            extract_wifi_eeprom(Path::new("Wi-Fi EEPROM"), data)?;
            Ok(PartStatus::Wifi { length: data.len() })
        }
    }
}

fn update_mac_field(
    image: &mut [u8],
    offset: usize,
    label: &str,
    value: &str,
    original: &str,
) -> Result<()> {
    if value == original {
        return Ok(());
    }
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    let mac = parse_mac(Path::new("identity"), label, value)?;
    place(image, offset, &mac, label)
}

pub struct Factory {
    image: Vec<u8>,
    pub mac: String,
    pub wifi_mac: String,
    pub wifi_mac2: String,
    pub pon_sn: String,
    pub device_sn: String,
}

pub struct OpenedFactory {
    pub factory: Factory,
    pub source_is_stock: bool,
    pub warning: Option<ImportWarning>,
}

impl Factory {
    pub fn new() -> Result<Self> {
        Ok(Self {
            image: vec![0xff; FACTORY_SIZE],
            mac: random_mac()?,
            wifi_mac: String::new(),
            wifi_mac2: String::new(),
            pon_sn: random_sn()?,
            device_sn: String::new(),
        })
    }
    pub fn open(path: &Path) -> Result<OpenedFactory> {
        if path.is_dir() {
            let mut files = crate::import::Files::new();
            for n in ["table", "factory_conf", "wlan/AX3000_RT30xxEEPROM_5.bin"] {
                let p = path.join(n);
                if p.is_file() {
                    files.insert(n.into(), read_file(&p, "file")?);
                }
            }
            return Ok(OpenedFactory {
                factory: Self::from_files(&files)?,
                source_is_stock: true,
                warning: None,
            });
        }
        let data = read_file(path, "image")?;
        if data.starts_with(b"UBI#") || data.starts_with(&[0x85, 0x19]) {
            let jffs2 = data.starts_with(&[0x85, 0x19]);
            let (files, needs_warning) = crate::import::extract(&data)?;
            return Ok(OpenedFactory {
                factory: Self::from_files(&files)?,
                source_is_stock: true,
                warning: needs_warning.then_some(if jffs2 {
                    ImportWarning::DamagedJffs2
                } else {
                    ImportWarning::DirtyUbifs
                }),
            });
        }
        Ok(OpenedFactory {
            factory: Self::from_image(data)?,
            source_is_stock: false,
            warning: None,
        })
    }
    pub fn from_image(image: Vec<u8>) -> Result<Self> {
        if image.len() != FACTORY_SIZE {
            return Err("Unified factory image must be exactly 1 MiB".into());
        }
        let mac = hex_mac(&image[BASE_MAC_OFFSET..BASE_MAC_OFFSET + BASE_MAC_SIZE]);
        let wifi_present = image[WIFI_OFFSET..WIFI_OFFSET + WIFI_SIZE]
            .iter()
            .any(|byte| *byte != 0xff);
        let wifi_mac = if wifi_present {
            hex_mac(&image[WIFI_OFFSET + 4..WIFI_OFFSET + 10])
        } else {
            String::new()
        };
        let wifi_mac2 = if wifi_present {
            hex_mac(&image[WIFI_OFFSET + 10..WIFI_OFFSET + 16])
        } else {
            String::new()
        };
        let sn = &image[PON_SN_OFFSET..PON_SN_OFFSET + PON_SN_SIZE];
        let pon_sn = if sn[..4].iter().all(u8::is_ascii_alphanumeric) {
            format!("{}{}", String::from_utf8_lossy(&sn[..4]), hex(&sn[4..]))
        } else {
            hex(sn)
        };
        let raw = &image[DEVICE_SN_OFFSET..DEVICE_SN_OFFSET + DEVICE_SN_SIZE];
        let end = raw
            .iter()
            .position(|b| *b == 0 || *b == 255)
            .unwrap_or(raw.len());
        let device_sn = String::from_utf8_lossy(&raw[..end]).into_owned();
        Ok(Self {
            image,
            mac,
            wifi_mac,
            wifi_mac2,
            pon_sn,
            device_sn,
        })
    }
    pub fn from_files(files: &crate::import::Files) -> Result<Self> {
        let config = files
            .get("factory_conf")
            .ok_or("Stock image has no factory_conf")?;
        let path = Path::new("factory_conf");
        let fields = parse_factory_conf(path, config)?;
        let mut image = vec![0xff; FACTORY_SIZE];
        if let Some(pon) = files.get("table") {
            place(
                &mut image,
                PON_OFFSET,
                &normalize_pon_calibration(Path::new("table"), pon)?,
                "PON calibration",
            )?;
        }
        if let Some(wifi) = files.get("wlan/AX3000_RT30xxEEPROM_5.bin") {
            place(
                &mut image,
                WIFI_OFFSET,
                &extract_wifi_eeprom(Path::new("Wi-Fi EEPROM"), wifi)?,
                "Wi-Fi EEPROM",
            )?;
        }
        let mac = parse_mac(path, "brmac", required_field(path, &fields, "brmac")?)?;
        let sn = parse_pon_serial(path, required_field(path, &fields, "GponSN")?)?;
        place(&mut image, BASE_MAC_OFFSET, &mac, "MAC")?;
        place(&mut image, PON_SN_OFFSET, &sn, "PON SN")?;
        if let Some(value) = fields.get("SerialNumber").filter(|s| !s.is_empty()) {
            place(
                &mut image,
                DEVICE_SN_OFFSET,
                &parse_device_serial(path, value)?,
                "device serial number",
            )?;
        }
        Self::from_image(image)
    }
    pub fn part(&self, p: Part) -> &[u8] {
        &self.image[p.range()]
    }
    pub fn part_present(&self, p: Part) -> bool {
        self.part(p).iter().any(|b| *b != 255)
    }
    pub fn part_status(&self, p: Part) -> PartStatus {
        if !self.part_present(p) {
            return PartStatus::Empty;
        }
        inspect_part(p, self.part(p)).unwrap_or(PartStatus::Invalid)
    }
    pub fn replace(&mut self, p: Part, path: &Path) -> Result<()> {
        let description = match p {
            Part::Pon => "PON calibration",
            Part::Wifi => "Wi-Fi EEPROM",
        };
        let data = read_file(path, description)?;
        let data = match p {
            Part::Pon => normalize_pon_calibration(path, &data)?,
            Part::Wifi => data,
        };
        inspect_part(p, &data)?;
        self.image[p.range()].copy_from_slice(&data);
        if matches!(p, Part::Wifi) {
            self.wifi_mac = hex_mac(&data[4..10]);
            self.wifi_mac2 = hex_mac(&data[10..16]);
        }
        Ok(())
    }
    pub fn clear(&mut self, p: Part) {
        self.image[p.range()].fill(255);
        if matches!(p, Part::Wifi) {
            self.wifi_mac.clear();
            self.wifi_mac2.clear();
        }
    }
    /// Applies edited identity fields to the loaded image.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let path = Path::new("identity");
        let mac = parse_mac(path, "base MAC", &self.mac)?;
        let sn = display_sn(&self.pon_sn)?;
        let mut image = self.image.clone();
        place(&mut image, BASE_MAC_OFFSET, &mac, "MAC")?;
        place(&mut image, PON_SN_OFFSET, &sn, "PON SN")?;
        let original = Self::from_image(self.image.clone())?;
        update_mac_field(
            &mut image,
            WIFI_OFFSET + 4,
            "2.4 GHz MAC",
            &self.wifi_mac,
            &original.wifi_mac,
        )?;
        update_mac_field(
            &mut image,
            WIFI_OFFSET + 10,
            "5 GHz MAC",
            &self.wifi_mac2,
            &original.wifi_mac2,
        )?;
        if original.device_sn != self.device_sn {
            let device = if self.device_sn.is_empty() {
                vec![0; DEVICE_SN_SIZE]
            } else {
                parse_device_serial(path, &self.device_sn)?
            };
            place(
                &mut image,
                DEVICE_SN_OFFSET,
                &device,
                "device serial number",
            )?;
        }
        Ok(image)
    }
}
fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02X}")).collect()
}
fn hex_mac(d: &[u8]) -> String {
    d.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}
fn display_sn(text: &str) -> Result<Vec<u8>> {
    if text.len() == 12
        && text.is_ascii()
        && text.as_bytes()[..4].iter().all(u8::is_ascii_alphanumeric)
    {
        let mut out = text.as_bytes()[..4].to_vec();
        out.extend(decode_hex(Path::new("PON SN"), "PON SN", &text[4..], 4)?);
        return Ok(out);
    }
    parse_pon_serial(Path::new("PON SN"), text)
}
pub fn random_mac() -> Result<String> {
    let mut data = [0u8; 6];
    getrandom::fill(&mut data).map_err(|e| e.to_string())?;
    // Set the locally administered bit and clear the multicast bit.
    data[0] = (data[0] & 0xfc) | 2;
    Ok(hex_mac(&data))
}
pub fn random_sn() -> Result<String> {
    let mut data = [0u8; 4];
    getrandom::fill(&mut data).map_err(|e| e.to_string())?;
    Ok(format!("FHTT{}", hex(&data)))
}
