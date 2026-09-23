# FiberHome Factory

Factory image editor for FiberHome AN758x ONTs. It imports stock UBI/UBIFS,
JFFS2, or unified factory images and writes the 1 MiB layout used by OpenWrt.

烽火 AN758x 光猫 Factory 镜像编辑器。支持导入原厂 UBI/UBIFS、JFFS2
以及统一 Factory 镜像，并输出 OpenWrt 使用的 1 MiB 布局。

Open or drag an image into the window, edit its identity or calibration data,
then save a new 1 MiB image.

打开或拖入镜像，编辑身份信息或校准数据，然后保存新的 1 MiB 镜像。

## Factory layout / Factory 布局

| Offset / 偏移 | Size / 大小 | Data / 内容 |
| --- | --- | --- |
| `0x000000` | `0x0800` | APONCAL PON calibration / APONCAL 光模块校准 |
| `0x000800` | `0x0800` | Reserved / 保留 |
| `0x001000` | `0x1000` | MT7916 EEPROM |
| `0x002000` | `0x0006` | Base MAC / 基础 MAC |
| `0x002006` | `0x000a` | Reserved / 保留 |
| `0x002010` | `0x0008` | PON serial number / PON 序列号 |
| `0x002018` | `0x0008` | Reserved / 保留 |
| `0x002020` | `0x0020` | Device serial number / 整机序列号 |
| `0x002040` | `0xfdfc0` | Reserved / 保留 |

## Build / 构建

Windows and macOS:

```powershell
cargo build --release --locked
```

Linux x86-64:

```sh
cargo build --release --locked --features linux-x11
```
