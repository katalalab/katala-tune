# Official hardware source coverage

Catalog revision 3 was reviewed on 2026-10-25. It contains 62 records: 33 product or product-family records and 29 manufacturer PCI device-ID/function mappings. This document lists public product identities and sources only; it contains no machine inventory or assignment of products to machines.

`models` contains explicit aliases matched case-insensitively after trimming. There is no substring, prefix, suffix, kit-name, or fuzzy matching. A match identifies the scope described by the source; it does not establish the installed configuration or enabled setting. Published maximum frequencies, transfer rates and endurance ratings are product specifications, not measurements. Every SSD performance field uses `up_to`; rated TBW is not remaining drive life.

Revision 3 also supports explicit `hardware_ids` with a section, four-digit hexadecimal vendor ID and device/product ID. These mappings have empty `models` arrays, so generic names cannot trigger a match. Their scope is the manufacturer-identified silicon family/function, separate from an exact product-model match. An ID mapping does not establish the motherboard SKU, stepping, board revision, enabled features or negotiated link.

## CPU, motherboards and networking

| Product / exact observed alias | Primary source | Catalog values and scope |
| --- | --- | --- |
| `Intel(R) Core(TM) Ultra 5 225` | [Intel desktop 225 specifications](https://www.intel.com/content/www/us/en/products/sku/241070/intel-core-ultra-5-processor-225-20m-cache-up-to-4-90-ghz/specifications.html) | 10 cores (6 P + 4 E), 10 threads, 4.9GHz maximum turbo, 20MB cache, 65W base / 121W turbo power, LGA1851. Does not cover the 225U, 225H or 225F. |
| `ROG STRIX Z790-F GAMING WIFI II` | [ASUS technical specifications](https://rog.asus.com/motherboards/rog-strix/rog-strix-z790-f-gaming-wifi-ii/spec/) | LGA1700 / Z790, four DDR5 slots, five storage M.2 slots, four SATA ports, 2.5Gb Ethernet and Wi-Fi 7 160MHz support. XMP support does not establish active XMP. |
| `B860M Pro RS WiFi` | [ASRock technical specifications](https://www.asrock.com/mb/Intel/B860M%20Pro%20RS%20WiFi/index.asp#Specification) | LGA1851 / B860, four DDR5 slots, three storage M.2 slots, four SATA ports, 2.5Gb Ethernet, Wi-Fi 6E and XMP 3.0 support. Covers the WiFi variant only. |
| `Intel(R) Wi-Fi 7 BE202 160MHz` | [Intel BE202 product brief, revision 1.2](https://cdrdv2-public.intel.com/787778/787778_Intel_Wi-Fi_7_BE202_MistyPeak2_Product_Brief_Rev1p2.pdf) | BE202-labelled introductory text confirms 2x2, 2.4/5/6GHz, 160MHz and theoretical 2.4Gbps. The technical table contains inconsistent BE200 labels; its package dimensions and module ordering codes are deliberately excluded. |
| `Intel(R) Wi-Fi 6E AX211 160MHz` | [Intel AX211 specifications](https://www.intel.com/content/www/us/en/products/sku/204837/intel-wifi-6e-ax211-gig/specifications.html) | 2x2 Wi-Fi 6E, 2.4/5/6GHz, 160MHz, theoretical 2.4Gbps and CNVio2. Does not assert current Bluetooth version, band availability or active link speed. |
| `Intel(R) Ethernet Controller I226-V` | [Intel I226-V specifications](https://www.intel.com/content/www/us/en/products/sku/210599/intel-ethernet-controller-i226v/specifications.html) | Single port, maximum 2.5Gbps, PCIe 3.1 and 1.3W TDP. Does not imply a 2.5Gbps negotiated connection or measured power. |

## Storage

| Exact identity | Primary source | Catalog values and scope |
| --- | --- | --- |
| `CT2000T500SSD8` | [Crucial T500 product flyer](https://www.crucial.com/content/dam/crucial/ssd-products/t500/flyers/b2c/crucial-T500-b2c-product-flyer-en_combination.pdf) | Exact 2TB SKU, PCIe 4.0 NVMe, no included heatsink, up to 7400/7000MB/s read/write, 1200TB rated endurance. |
| `CT1000MX500SSD1` | [Crucial MX500 product flyer, Spanish](https://content.crucial.com/content/dam/crucial/ssd-products/mx500/flyer/crucial-mx500-ssd-productflyer-es_es.pdf) | Exact 1TB SKU, SATA 6Gb/s, 2.5-inch / 7mm, up to 560/510MB/s read/write, 360TB rated endurance. |
| `CT4000MX500SSD1` | [Crucial MX500 product flyer, Spanish](https://content.crucial.com/content/dam/crucial/ssd-products/mx500/flyer/crucial-mx500-ssd-productflyer-es_es.pdf) | Exact 4TB SKU, SATA 6Gb/s, 2.5-inch / 7mm, up to 560/510MB/s read/write, 1000TB rated endurance. |
| `CT2000BX500SSD1` | [Crucial BX500 product flyer](https://www.crucial.com/content/dam/crucial/ssd-products/bx500/flyer/b2c/crucial-bx500-b2c-product-flyer-en.pdf) | Exact 2TB SKU, SATA 6Gb/s, 2.5-inch / 7mm, up to 540/500MB/s read/write, 720TB rated endurance. Controller/NAND revision is not inferred. |
| `CT1000P3PSSD8` | [Crucial P3 Plus product flyer](https://www.crucial.com/content/dam/crucial/ssd-products/p3-plus/flyers/b2c/crucial-p3plus-productflyer-consumer.pdf) | Exact 1TB SKU, up to 5000/3600MB/s read/write. Does not match the separate P3 SKU `CT1000P3SSD8`. |
| `INTEL SSDPEKKW010T8` | [Intel PCN 117167-00](https://cdrdv2-public.intel.com/808187/PCN117167-00.pdf), [Intel declaration of conformity](https://www.intel.com/content/dam/www/public/us/en/documents/declaration-of-conformity/solid-state-drives/ssd-7600p-760p-e6100p-declaration.pdf) | PCN identifies ordering code `SSDPEKKW010T8X1` as 760p 1.024TB, M.2 80mm, PCIe 3.0 x4, TLC. Declaration explicitly lists base model `SSDPEKKW010T8`. The similar `SSDPEKNW` family is not used. |
| `NVMe WD_BLACK SN770 1TB` | [WD_BLACK SN770 manufacturer product brief](https://documents.sandisk.com/content/dam/asset-library/en_us/assets/public/western-digital/product/internal-drives/wd-black-ssd/product-brief-wd-black-sn770-nvme-ssd.pdf) | 1TB SKU `WDS100T3X0E`, PCIe Gen4 x4, M.2 2280, up to 5150/4900MB/s read/write, 600TB rated endurance. Explicit full aliases only; no match to SN770M or other capacities. |
| `WDC WDS240G2G0A-00JH30` | [WD Green manufacturer product brief](https://documents.sandisk.com/content/dam/asset-library/en_us/assets/public/western-digital/product/internal-drives/wd-green-ssd/product-brief-wd-green-ssd.pdf), [manufacturer Dashboard manual](https://documents.sandisk.com/content/dam/asset-library/en_us/assets/public/western-digital/product/internal-drives/wd-green-ssd/user-manual-ssd-dashboard-4779-705161.pdf) | Brief identifies base SKU `WDS240G2G0A` as 240GB, 2.5-inch / 7mm SATA, up to 545MB/s read. The manual's screenshots show the exact suffix and WDC-prefixed model string. Example firmware and current connection values are not copied. |

The five Crucial SSD entries remain checked against indexed text of manufacturer-hosted flyer tables. Revision 3 retried the original files, publicly indexed Crucial regional copies and the current Micron assets distribution URLs. The official Micron [T500 flyer](https://assets.micron.com/adobe/assets/urn%3Aaaid%3Aaem%3A6b046d5a-4382-4018-b81e-28421f19b2cc/renditions/original/as/crucial-T500-b2c-product-flyer-en-combination.pdf), [MX500 flyer](https://assets.micron.com/adobe/assets/urn%3Aaaid%3Aaem%3A3c743a1f-9319-4b34-9b20-15cdad1ee2ce/renditions/original/as/crucial-mx500-ssd-productflyer-en.pdf.html), [BX500 flyer](https://assets.micron.com/adobe/assets/urn%3Aaaid%3Aaem%3A89a539f7-d1ba-4202-ab4d-161406b8747f/renditions/original/as/crucial-bx500-ssd-productflyer.pdf.html) and [P3 Plus flyer](https://assets.micron.com/adobe/assets/urn%3Aaaid%3Aaem%3Aac83bf68-de9d-4725-a7c1-476ba03505b8/renditions/original/as/crucial-p3plus-productflyer-consumer.pdf) were found, but native HTTPS retrieval returned a 269-byte request-rejection HTML body, not a PDF. `content.crucial.com` failed native Windows certificate validation with an expired-certificate error. The [Micron downloads page](https://www.micron.com/sales-support/downloads) was retrieved and inspected; its current public resources did not provide a working alternate flyer body. Search of the older `media-www.micron.com` distribution host found no matching official flyer.

Certificate validation remained enabled and no browser challenge or access control was bypassed. No rejection body is labelled a downloaded document, and no reseller copy substitutes for a manufacturer-hosted source. All five entries retain `source_access: manufacturer_document_indexed_excerpt` and their visible access limit. Their existing numerical claims were not expanded from the newer indexed excerpts. Full-body retrieval for these five entries remains unresolved.

## Memory module revisions

| Exact revision | Primary source | Scope |
| --- | --- | --- |
| `CT32G48C40U5.M16A1` | [ASUS PRIME H610M-K ARGB memory QVL](https://www.asus.com/motherboards-components/motherboards/prime/prime-h610m-k-argb/helpdesk_qvl_memory?model2Name=PRIME-H610M-K-ARGB) | ASUS explicitly lists this full revision as a 32GB DDR5 module with listed rate 4800. Catalog retains only capacity/type/rate. |
| `CT32G56C46U5.C16B2` | [ASRock Industrial IMB-X1715 memory QVL](https://download.asrock.com/IPC/List/Memory/IMB-X1715.pdf) | ASRock explicitly lists this full revision as a 32GB DDR5 module with listed rate 5600. Catalog retains only capacity/type/rate. |

These are primary motherboard-manufacturer compatibility lists, not Crucial module datasheets. Matching is restricted to their full listed revision strings. The catalog names and scope expose that source relationship. The QVL's tested board is not the observed board, and no compatibility promise is transferred to another motherboard. Rank, memory-chip revision, timings and active profiles must come from observed SPD/firmware or an appropriate part datasheet.

## Displays and camera

| Exact product name | Primary source | Catalog values and scope |
| --- | --- | --- |
| `DELL S3422DWG` | [Dell S3422DWG technical specifications](https://www.dell.com/en-us/shop/dell-34-curved-gaming-monitor-s3422dwg/apd/210-azep/monitors-monitor-accessories) | 34-inch VA, 3440×1440, DisplayPort up to 144Hz / HDMI up to 100Hz, 1800R curve. Dell explicitly says no built-in speaker. An audio endpoint name is not evidence of speakers. |
| `EV2750` | [EIZO EV2750 product announcement](https://www.eizoglobal.com/press/releases/htmls/ev2750.html), [complete manufacturer user manual](https://www.eizoglobal.com/support/db/files/manuals/03V25463D1/UM-03V25463D1-EN.pdf) | The complete announcement body confirms 27-inch IPS, LED backlight and native 2560×1440. The complete 74-page manual was also downloaded/extracted; section 6-5, printed/PDF page 48 independently confirms all four catalog values and was visually checked. `source_access` is now `manufacturer_page_full_body`. |
| `Logitech StreamCam` | [Logitech StreamCam product page](https://www.logitech.com/en-us/shop/p/streamcam) | Full HD 1080p at up to 60fps, 78° view and USB-C. Family-level camera capabilities only; does not infer color/region SKU or active audio/camera modes. |

Product names can appear in display, USB and audio sections. A name match links the product's published capabilities and never turns one endpoint's observation into a measurement of all functions.

## Audio and mouse additions

| Identity | Primary source | Verified scope and access |
| --- | --- | --- |
| `RME Babyface Pro` | [RME original product archive](https://archiv.rme-audio.de/en/products/babyface_pro.php), [Babyface Pro FS product page](https://rme-audio.de/babyface-pro-fs.html) | Complete HTML bodies from both manufacturer pages were downloaded and read. Only shared original/FS family capabilities are retained: up to 12 inputs/12 outputs, up to 192kHz and USB 2.0. Maximum channel count depends on sample rate and external digital equipment. The USB name does not establish FS revision, current clock, latency or power. |
| `Razer Viper 8KHz` | [Razer support specifications, RZ01-03580](https://mysupport.razer.com/app/answers/detail/a_id/4007) | The exact-name indexed manufacturer specification table confirms up to 8000Hz polling, 20000DPI, eight programmable buttons and 1.8m cable. Native retrieval returned a browser challenge, so `manufacturer_page_indexed_excerpt` records that limitation; active settings and observed polling are not asserted. |

## Exact PCI function identities

All rows below match vendor `8086`, section `pci`, and only the explicit device IDs listed. The complete manufacturer HTML pages and relevant device-ID tables were retrieved and read. Each catalog record contains a SHA-256 of that page body and the checked row in `source_evidence.notes`. These are functional identities; maximum USB/link capabilities and stepping columns are deliberately excluded.

| Manufacturer document | Exact device IDs and functions included |
| --- | --- |
| [Intel 700 Series PCH Device and Revision ID](https://edc.intel.com/content/www/us/en/design/products-and-solutions/processors-and-chipsets/700-series-chipset-family-platform-controller-hub-datasheet-volume-1-of/device-and-revision-id/), public document 743835 revision 005 | `7A04`, `7A06`: eSPI; `7A23`: SMBus; `7A24`: SPI flash; `7A27`: shared SRAM; `7A30/34/36/38/3B/40/48`: PCIe root ports 9/13/15/1/4/17/25 respectively; `7A4C/4D/4E`: I2C controllers 0/1/2; `7A60`: xHCI; `7A62`: SATA AHCI; `7A68`: CSME HECI 1. Eighteen function records. |
| [Intel 800 Series PCH Device IDs](https://edc.intel.com/content/www/us/en/design/products/platforms/details/arrow-lake-s/800-series-chipset-family-platform-controller-hub-pch-datasheet-volume/device-ids/), public document 833778 revision 003 | `7F06`: eSPI; `7F23`: SMBus; `7F24`: SPI flash; `7F36/37`: PCIe root ports 15/16; `7F4C/4E`: I2C controllers 0/2; `7F50`: Audio Context Engine; `7F62`: SATA AHCI; `7F68`: HECI 1; `7F6E`: xHCI. Eleven function records. |

The device-ID tables identify the function; their revision columns do not establish an observed device's stepping. IDs absent from these tables, generic names without IDs, and IDs belonging to other silicon families remain unverified. In particular, no mapping is inferred for `7F2F`, the `AE..` functions, or an NVIDIA audio/controller function merely because it accompanies a known GPU.

## Full-body evidence

`source_evidence` contains the exact `url`, lowercase hexadecimal `sha256`, `coverage`, `verified_at` and concise `notes`. For HTML, `coverage: full_page` means the entire returned source page was saved and its relevant content read; it does not mean every document linked from that page was downloaded. A future PDF entry uses `coverage: full_document` and a `pages` array locating the claims within the completely downloaded PDF. No local machine path is included in public metadata.

The EIZO announcement body SHA-256 is `bd0b935195aec9d8d415496ffbb5ebda372490949259dfb38985699979c3e738`. Its independent manual verification is section 6-5, page 48, in a fully parsed 74-page PDF. The two Intel page-body hashes are `ba9d4658157fc2794f2eb82498b32471e206d67ee2abe0afbd008c6f66642fbb` (700 Series) and `a8eec80fc6bc77ffb5d2959c1f9a7577791de2ccb5b661edbe9f3f4e60ca9e3e` (800 Series). Public downloads, extracted text, page rendering and retrieval-attempt provenance are retained outside Git. Existing revision-1/2 sources without full-body metadata are not newly represented as fully retrieved.

## Deliberately unresolved identities

| Name or class | Why no exact catalog claim is made |
| --- | --- |
| `F4-3600C19-16GSXWB` | [G.SKILL documents the kit `F4-3600C19D-32GSXWB`](https://www.gskill.com/product/165/169/1536045873/F4-3600C19D-32GSXWB), but a manufacturer statement tying the exact individual-module string to that kit was not established. Kit timings/voltage must not be transferred by spelling similarity. |
| `VG252Q` | Acer publishes different [VG252Q P](https://www.acer.com/tr-tr/monitors/gaming/nitro-vg2/pdp/UM.KV2EE.P01) and [VG252Q X](https://www.acer.com/gb-en/monitors/gaming/nitro-vg2/pdp/UM.KV2EE.X01) variants, with other suffixes also documented. The truncated name and EDID product ID were not tied to one exact suffix by a manufacturer mapping. No suffix-specific refresh-rate or port claim is attached. |
| `OWC USB-C Dock` | No exact generation or ordering code. OWC has multiple USB-C dock products with different ports and power delivery; a family name cannot determine the SKU. |
| `iLok` | Generation, connector and ordering code are missing. A generic USB name cannot distinguish hardware revisions. |
| `LIGHTSPEED`, broad Logitech/USB receiver labels | A shared transport/receiver brand is not an exact mouse, keyboard or headset model. |
| `APPLE SSD AP0512Z`, `APPLE SSD AP2048Z`, `Apple SSD Controller` | Internal Apple part/controller names are not sufficiently mapped by the reviewed public manufacturer specifications. Do not infer a retail SSD model, NAND generation or performance limit. |
| `Unified memory`, `memory`, `3000 C16 Series` | Generic/family labels lack an exact public module SKU. CPU-family maximum memory is not a specification for the installed DIMM. |
| `Realtek Gaming 2.5GbE Family Controller`, generic TP-Link USB adapters | A family/driver string can cover multiple physical models or revisions. Need PCI/USB IDs plus a verified manufacturer mapping or exact SKU. |
| Virtual audio/network adapters, OS names, generic PCI bridges, system firmware, `Color LCD` | These labels do not identify an exact physical product with a directly attributable hardware datasheet. Keep observed fields and missing-data reasons; do not manufacture a match. |

## Link trust and maintenance

Catalog revision 3 additionally needs manufacturer roots `rme-audio.de` and `razer.com`, covering `archiv.rme-audio.de`, `rme-audio.de` and `mysupport.razer.com`. Existing manufacturer roots cover `crucial.com`, `sandisk.com`, `dell.com`, `eizoglobal.com`, `logitech.com`, `cdrdv2-public.intel.com`, `edc.intel.com`, `rog.asus.com` and `download.asrock.com`. Domains researched but not used for catalog records do not need link-trust expansion.

Future updates should preserve exact aliases, record the source scope, and distinguish a capability from a current setting. A changed part suffix requires evidence; it must not be accepted by stripping the suffix. Existing revision-1 records retain their sources and are not represented as newly revalidated by this expansion.
