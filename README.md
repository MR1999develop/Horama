# Horama

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-2021%20Edition-orange.svg)](https://www.rust-lang.org/)
[![Slint UI](https://img.shields.io/badge/GUI-Slint%20v1.9-purple.svg)](https://slint.dev/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-lightgrey.svg)]()

**Horama** è un visualizzatore di immagini e file Camera RAW ad alte prestazioni sviluppato in **Rust** e **Slint UI**, progettato per fotografi e professionisti dell'imaging con gestione avanzata dei profili colore ICC e interfaccia darkroom moderna.

---

## Caratteristiche Principali

- **Formati Supportati**:
  - **Raster standard**: JPEG, PNG, TIFF (8 e 16 bit), WebP, BMP, GIF, ICO, TGA.
  - **Camera RAW**: Canon (`.CR2`, `.CR3`), Nikon (`.NEF`), Sony (`.ARW`), Adobe (`.DNG`), Fujifilm (`.RAF`), Olympus (`.ORF`), Panasonic (`.RW2`), Pentax (`.PEF`) e altri.
- **Navigazione & Prestazioni**:
  - Estrazione sub-millisecondo dei thumbnail EXIF IFD1 integrati nei file RAW/JPEG.
  - Cache RAM predittiva multi-gigabyte con decodifica in background.
  - Albero cartelle gerarchico espandibile/comprimibile con ripristino dell'ultima sottocartella all'avvio.
  - Rullino orizzontale scorrevole con miniature sincronizzate in tempo reale.
- **Editing & Precisione**:
  - Rotazione immagini (90° oraria, antioraria, 180°) salvata automaticamente su disco e sincronizzata nel rullino.
  - Zoom al 100% centrato sul cursore del mouse con doppio click sinistro e modalità Adatta (*Fit*).
  - Lente d'ingrandimento 4× (pressione tasto centrale del mouse) e Contagocce colore RGB/HSL (pressione tasto destro).
  - Ridenominazione file singola e multipla (F2) conforme agli standard di sistema.
  - Menu contestuale nativo *"Apri con"* con le applicazioni registrate nel sistema operativo.
  - Istogramma in tempo reale (RGB e Luminanza) e metadati fotografici completi EXIF.
  - Filtri istantanei per testo/nome parziale e valutazione a stelle (★).
- **Gestione Colore ICC**:
  - Motore `qcms` con supporto a profili incorporati, sRGB, Adobe RGB, ProPhoto RGB, Display P3, Rec.2020 e profili ICC/ICM personalizzati.
- **Integrazione MCP (Model Context Protocol)**:
  - Server MCP nativo su `stdio` per consentire a **Claude** o **Gemini** di controllare direttamente l'applicazione (apertura cartelle, selezione immagini, rotazione, zoom, filtri e lettura metadati).

---

## Avvio Rapido

### Prerequisiti
- **Rust toolchain** (1.75+): [rustup.rs](https://rustup.rs/)
- **C Compiler (MinGW-w64 su Windows, GCC/Clang su Linux)**

### Esecuzione su Windows
Tramite lo script di avvio rapido:
```bat
run_windows.bat
```
Oppure tramite Cargo:
```powershell
cargo run --release
```

### Avvio in modalità Server MCP (per Claude Desktop / Gemini)
Aggiungere il flag `--mcp`:
```powershell
horama.exe --mcp
```
*(Vedi [`mcp_client_config.json`](mcp_client_config.json) per la configurazione con Claude Desktop).*

---

## Scorciatoie da Tastiera Predefinite

Tutte le scorciatoie sono completamente personalizzabili dall'applicazione tramite il menu **`Help -> Keyboard Shortcuts...`**.

| Azione | Scorciatoia Predefinita |
| :--- | :--- |
| **Immagine Successiva** | `Freccia Destra` |
| **Immagine Precedente** | `Freccia Sinistra` |
| **Zoom In / Zoom Out** | `+` (o `=`) / `-` (o `_`) |
| **Adatta alla Finestra** | `Ctrl+0` |
| **Zoom 100% (1:1)** | `Ctrl+1` (o *Doppio Click Sinistro*) |
| **Ruota Oraria (90°)** | `R` |
| **Ruota Antioraria (90°)** | `L` |
| **Rinomina File** | `F2` |
| **Elimina Selezionate** | `Delete` |
| **Aggiorna Cartella** | `F5` |
| **Apri File(s)...** | `Ctrl+O` |
| **Apri Cartella...** | `Ctrl+Shift+O` |
| **Salva / Esporta...** | `Ctrl+S` |
| **Valutazione (Rating)**| Tasti numerici `0` - `5` |

---

## Licenza

Progetto open-source rilasciato sotto licenza **GNU General Public License v3.0 (GPL-3.0-or-later)**. Consulta il file [LICENSE](LICENSE) per i dettagli.
