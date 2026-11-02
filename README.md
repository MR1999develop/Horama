# Horama

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-2021%20Edition-orange.svg)](https://www.rust-lang.org/)
[![Slint UI](https://img.shields.io/badge/GUI-Slint%20v1.9-purple.svg)](https://slint.dev/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-lightgrey.svg)]()

**Horama** (dal greco ὅραμα, "ciò che si vede"; in precedenza *Horama*) è un visualizzatore di immagini e file Camera RAW ad alte prestazioni sviluppato in **Rust** e **Slint UI**, progettato per fotografi e professionisti dell'imaging con gestione avanzata dei profili colore ICC e interfaccia darkroom moderna.

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
  - Rotazione immagini (90° oraria, antioraria, 180°) salvata automaticamente su disco **senza perdita di qualità**:
    - **JPEG / TIFF**: viene aggiornato solo il tag di orientamento EXIF/TIFF (e XMP `tiff:Orientation`); i dati compressi e tutti i metadati restano identici byte per byte.
    - **PNG / WebP / BMP**: i pixel vengono ricodificati con encoder lossless (PNG mantiene 16 bit e trasparenza; profilo ICC, EXIF e testi vengono conservati).
    - **RAW**: la rotazione è solo a schermo, i file RAW non vengono mai modificati.
    - Le scritture sono atomiche (file temporaneo + rinomina) e serializzate; all'uscita l'app attende il completamento delle scritture in corso.
  - Ritaglio non distruttivo in memoria: salvataggio esplicito con *Salva / Esporta* (o "salva tutto" all'uscita, con nomi sequenziali `nome_1.jpg`).
  - Zoom al 100% centrato sul cursore del mouse con doppio click sinistro e modalità Adatta (*Fit*).
  - Lente d'ingrandimento 4× (pressione tasto centrale del mouse) e Contagocce colore RGB/HSL (pressione tasto destro).
  - Ridenominazione file singola e multipla (F2) conforme agli standard di sistema: l'estensione originale viene sempre mantenuta, nessun file esistente viene mai sovrascritto e in caso di errore tutte le rinomine vengono annullate.
  - Eliminazione nel **Cestino** (Windows: Cestino di sistema; macOS: Finder; Linux: cestino freedesktop), senza conferma: un avviso nella vista offre "Undo" per qualche secondo; i dischi senza Cestino chiedono conferma prima di eliminare definitivamente.
  - Apertura di un'immagine da Esplora file ("Apri con…") o trascinandola nella finestra: si apre la sua cartella, con l'immagine selezionata e la cartella evidenziata nell'albero a sinistra. Trascinando più immagini si apre solo quella selezione; trascinando una cartella si apre la cartella.
  - Menu contestuale nativo *"Apri con"* con le applicazioni registrate nel sistema operativo.
  - Istogramma in tempo reale (RGB e Luminanza) e metadati fotografici completi EXIF.
  - Filtri istantanei per testo/nome parziale e valutazione a stelle (★).
- **Gestione Colore ICC**:
  - Motore `qcms` con supporto a profili incorporati, sRGB, Adobe RGB, ProPhoto RGB, Display P3, Rec.2020 e profili ICC/ICM personalizzati.
  - Immagini **con profilo incorporato**: scegliere un altro profilo esegue una *conversione* (incorporato → scelto → monitor), come "Converti in profilo" di Photoshop. L'aspetto a schermo resta lo stesso (cambia solo se lo spazio scelto è più piccolo e alcuni colori vanno tagliati), mentre cambiano i valori dei pixel, l'istogramma, il contagocce e il file esportato. Immagini **senza profilo**: il profilo scelto viene *assegnato* e l'aspetto cambia.
  - Profilo del monitor: Windows (per monitor, aggiornato quando la finestra cambia schermo), Linux tramite `colord` (`colormgr`), macOS trattato come sRGB.
  - Pipeline di visualizzazione a 8 bit per canale (i file a 16 bit vengono letti e mostrati a 8 bit; la rotazione lossless dei PNG a 16 bit conserva invece la profondità originale).
  - Esportazione in JPEG, PNG, TIFF 8 bit, WebP lossless e BMP con profilo ICC ed EXIF originale (orientamento azzerato, dimensioni aggiornate). L'esportazione DNG non è supportata.
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
*(Vedi [`mcp_client_config.json`](mcp_client_config.json) per la configurazione con Claude Desktop: sostituire `C:\\path\\to\\horama.exe` con il percorso reale dell'eseguibile, ad es. `...\\target\\release\\horama.exe`).*

In alternativa al flag si può impostare la variabile d'ambiente `HORAMA_MCP` (è ancora accettata la vecchia `HORAMA_MCP`).

Strumenti MCP disponibili: `open_folder`, `select_image`, `rotate_image` (rotazione lossless come sopra), `set_zoom`, `set_rating` (0–5), `filter_images` (testo, valutazione minima, formato `JPG`/`PNG`/`TIFF`/`WEBP`/`RAW`), `get_current_image_info`, `list_images`.

---

## Scorciatoie da Tastiera Predefinite

Tutte le scorciatoie sono completamente personalizzabili dall'applicazione tramite il menu **`Help -> Keyboard Shortcuts...`**.

| Azione | Scorciatoia Predefinita |
| :--- | :--- |
| **Immagine Successiva** | `Freccia Destra` |
| **Immagine Precedente** | `Freccia Sinistra` |
| **Zoom In / Zoom Out** | `+` (o `=`) / `-` (o `_`) |
| **Adatta alla Finestra** | `Ctrl+0` (o `Spazio`) |
| **Zoom 100% (1 pixel immagine = 1 pixel schermo)** | `Ctrl+1` (o *Doppio Click Sinistro*) |
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

## Nuova interfaccia egui (anteprima)

È in costruzione una nuova interfaccia basata su [egui](https://github.com/emilk/egui), accanto a quella attuale in Slint. Le due interfacce condividono la libreria del programma (decodifica, gestione colore, file, filtri e selezione, geometria della vista).

```
cargo run --release --bin horama-egui
```

Per ora apre una cartella (menu, riga di comando o trascinamento), mostra l'albero delle cartelle e la filmstrip con le miniature, con ricerca e selezione multipla, visualizza l'immagine corrente con gestione colore, "adatta alla finestra", 100% (Ctrl+1), zoom sul cursore e spostamento, e precarica le immagini vicine. Le valutazioni si danno con i tasti 0–5 o con le stelle nella barra di stato; i filtri (stelle, formato, proporzioni, profilo colore) sono nel pannello a sinistra. Modifiche, metadati e gli altri strumenti arriveranno nelle fasi successive.

## File di log

Horama scrive un file di log, utile per capire un problema anche nella versione release (che non ha console):

- **Windows:** `%APPDATA%\Horama\logs\horama.log`
- **Linux:** `~/.config/Horama/logs/horama.log`
- **macOS:** `~/Library/Application Support/Horama/logs/horama.log`

La prima riga di ogni avvio riporta versione, commit e sistema operativo. Oltre 1 MB il file viene ruotato (`horama.1.log`, `horama.2.log`, `horama.3.log`; i più vecchi vengono cancellati). Il livello predefinito è `info`; la variabile d'ambiente `HORAMA_LOG` (`error`, `warn`, `info`, `debug`, `trace`) lo cambia. Il log contiene percorsi di file e cause degli errori, mai il contenuto delle immagini.

## Aggiornamento da Horama

Al primo avvio Horama copia le impostazioni della versione precedente (valutazioni, cartelle recenti, scorciatoie, dimensioni della finestra) dalla cartella `Horama` alla cartella `Horama` nelle impostazioni dell'utente (`%APPDATA%` su Windows, `~/.config` su Linux). I file vecchi restano dove sono. Nella configurazione MCP di Claude Desktop vanno aggiornati il nome del server (`horama`) e il percorso dell'eseguibile.

---

## Licenza

Progetto open-source rilasciato sotto licenza **GNU General Public License v3.0 (GPL-3.0-or-later)**. Consulta il file [LICENSE](LICENSE) per i dettagli.
