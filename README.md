# Horama

[![License: GPL-3.0-only](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-orange.svg)](https://www.rust-lang.org/)
[![egui](https://img.shields.io/badge/GUI-egui%200.37-purple.svg)](https://github.com/emilk/egui)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-lightgrey.svg)]()

**Horama** (dal greco ὅραμα, "ciò che si vede") è un visualizzatore di immagini e file Camera RAW ad alte prestazioni sviluppato in **Rust** ed **egui**, progettato per fotografi e professionisti dell'imaging con gestione avanzata dei profili colore ICC e interfaccia darkroom moderna.

---

## Caratteristiche Principali

- **Formati Supportati**:
  - **Raster standard**: JPEG, PNG, TIFF (8 e 16 bit), WebP, BMP, GIF, ICO, TGA.
  - **Camera RAW**: Canon (`.CR2`, `.CR3`), Nikon (`.NEF`), Sony (`.ARW`), Adobe (`.DNG`), Fujifilm (`.RAF`), Olympus (`.ORF`), Panasonic (`.RW2`), Pentax (`.PEF`) e altri.
- **Navigazione & Prestazioni**:
  - Estrazione sub-millisecondo dei thumbnail EXIF IFD1 integrati nei file RAW/JPEG.
  - Cache RAM predittiva multi-gigabyte con decodifica in background.
  - Albero cartelle gerarchico espandibile/comprimibile con ripristino dell'ultima sottocartella all'avvio.
  - Rullino orizzontale con miniature sincronizzate in tempo reale, che scorre anche con la rotella del mouse.
- **Editing & Precisione**:
  - Rotazione immagini (90° oraria, antioraria, 180°) salvata automaticamente su disco **senza perdita di qualità**:
    - **JPEG / TIFF**: viene aggiornato solo il tag di orientamento EXIF/TIFF (e XMP `tiff:Orientation`); i dati compressi e tutti i metadati restano identici byte per byte.
    - **PNG / WebP / BMP**: i pixel vengono ricodificati con encoder lossless (PNG mantiene 16 bit e trasparenza; profilo ICC, EXIF e testi vengono conservati).
    - **RAW**: la rotazione è solo a schermo, i file RAW non vengono mai modificati.
    - Le scritture sono atomiche (file temporaneo + rinomina) e serializzate; all'uscita l'app attende il completamento delle scritture in corso.
  - Ritaglio non distruttivo in memoria: salvataggio esplicito con *Salva / Esporta* (o "salva tutto" all'uscita, con nomi sequenziali `nome_1.jpg`).
  - Zoom al 100% centrato sul cursore del mouse con doppio click sinistro e modalità Adatta (*Fit*).
  - Lente d'ingrandimento rotonda 6× (pressione tasto centrale del mouse) e Contagocce colore RGB/HSL (pressione tasto destro).
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
- **Rust toolchain** 1.99 (la versione è fissata in `rust-toolchain.toml`): [rustup.rs](https://rustup.rs/)
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

In alternativa al flag si può impostare la variabile d'ambiente `HORAMA_MCP`.

Strumenti MCP disponibili: `open_folder`, `select_image`, `rotate_image` (rotazione lossless come sopra), `set_zoom`, `set_rating` (0–5), `filter_images` (testo, valutazione minima da −1 = solo senza valutazione a 5, formato `JPG`/`PNG`/`TIFF`/`WEBP`/`RAW`), `get_current_image_info`, `list_images`.

Il server tratta il client come non fidato: ogni chiamata viene controllata (campi, valori, lunghezze) prima di arrivare all'interfaccia, e una chiamata non valida riceve un errore senza effetti. L'unico strumento che modifica file è `rotate_image`, senza perdita di qualità; i RAW non vengono mai modificati.

### Sviluppo
Prima di ogni merge in `main` si esegue un solo comando, lo stesso che esegue la CI:
```powershell
scripts\check.bat
```
(`scripts/check.sh` su Linux e macOS). Controlla formattazione, clippy, test, dipendenze e build di rilascio. Il controllo delle dipendenze (vulnerabilità note, licenze compatibili con GPL-3.0, sorgenti) usa `cargo-deny`, da installare una volta con `cargo install cargo-deny --locked`.

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

## Interfaccia

L'interfaccia è basata su [egui](https://github.com/emilk/egui).

```
cargo run --release
```

Cosa fa:

- apre una cartella o un file (menu, cartelle recenti, riga di comando, trascinamento; trascinando più immagini apre solo quelle), con l'albero delle cartelle (riapre l'ultima sottocartella) e la filmstrip delle miniature, ricerca e selezione multipla; la cartella si aggiorna da sola quando altri programmi aggiungono, cancellano o rinominano file, e le miniature di quei file portano il contrassegno Nuovo, Modificato o Rinominato (avvisa anche se l'immagine aperta viene eliminata); il pannello delle cartelle si nasconde da Visualizza > Cartelle;
- mostra l'immagine con gestione colore (profilo del monitor aggiornato quando la finestra passa a un altro schermo), adatta alla finestra, 100%, zoom sul cursore, livelli di zoom predefiniti (10%–800%) e spostamento; il doppio clic passa al 100% nel punto cliccato e torna ad adattare; oltre il 100% i pixel si vedono netti; lente rotonda 6× (tasto centrale) e contagocce RGB/HSL (tasto destro);
- precarica le immagini vicine (lo stato della cache RAM è nella barra di stato); un solo caricamento alla volta, così scorrendo veloce si decodifica solo l'immagine su cui ci si ferma;
- valutazioni (0–5, stelle nella barra di stato o sulle miniature) e tutti i filtri (stelle, formato, proporzioni, profilo colore, profilo cambiato, ritagliate);
- pannello informazioni con istogramma, gestione colore (profilo incorporato, profilo in uso, monitor) e metadati (I);
- scelta e conversione del profilo colore (menu Immagine, P per il successivo) e profili ICC/ICM personalizzati;
- ritaglio (C) con proporzioni libere o con quelle del filtro "Proporzioni" (anche quelle personalizzate, che si possono aggiungere dalla barra del ritaglio e restano salvate), cornice ruotabile di 90° (X o il pulsante nella barra), Annulla/Ripeti (Ctrl+Z / Ctrl+Y) per ritagli, valutazioni, profili e rotazioni, salvataggio dei ritagli come nuovi file accanto agli originali (Ctrl+S) oppure sopra l'originale con File > Sovrascrivi l'originale (non per i RAW, sempre dopo una conferma; i JPEG sono compressi come l'originale) e richiesta alla chiusura se restano ritagli non salvati;
- confronto fino a nove immagini selezionate, affiancate;
- rotazione senza perdita (R / L; i RAW e i formati che non si possono riscrivere senza perdita ruotano solo a schermo), Cestino senza conferma con Annulla per 8 secondi (Canc), rinomina di uno o più file (F2), "Salva una copia come" con la scelta della qualità JPEG (Ctrl+Maiusc+S), "Apri con" dal menu del tasto destro sulla filmstrip;
- scorciatoie personalizzabili (Visualizza > Scorciatoie da tastiera), F6 per passare da un pannello all'altro;
- barra degli strumenti con blocco dello zoom e del profilo colore;
- aspetto uniforme a Windows 11: colori di Windows 11, Segoe UI e icone di sistema; tema Sistema / Chiaro / Scuro e lingua (inglese predefinito, italiano, tedesco, francese, spagnolo, cinese o quella del sistema) in File > Preferenze (Ctrl+,); il cinese usa un font di sistema (Microsoft YaHei, PingFang, Noto Sans CJK); menu Aiuto con scorciatoie e informazioni; nomi e stati dei controlli per i lettori di schermo;
- server MCP (`horama --mcp`).

Le valutazioni sono salvate nel file `ratings.jsonl`, accanto a `settings.json` nella cartella delle impostazioni. Alla prima apertura le valutazioni salvate dalle versioni precedenti dentro `settings.json` vengono spostate lì; una copia del file com'era resta in `settings.json.before-ratings`.

## File di log

Horama scrive un file di log, utile per capire un problema anche nella versione release (che non ha console):

- **Windows:** `%APPDATA%\Horama\logs\horama.log`
- **Linux:** `~/.config/Horama/logs/horama.log`
- **macOS:** `~/Library/Application Support/Horama/logs/horama.log`

La prima riga di ogni avvio riporta versione, commit e sistema operativo. Oltre 1 MB il file viene ruotato (`horama.1.log`, `horama.2.log`, `horama.3.log`; i più vecchi vengono cancellati). Il livello predefinito è `info`; la variabile d'ambiente `HORAMA_LOG` (`error`, `warn`, `info`, `debug`, `trace`) lo cambia. Il log contiene percorsi di file e cause degli errori, mai il contenuto delle immagini.

---

## Sicurezza

Le vulnerabilità si segnalano in privato, come descritto in [SECURITY.md](SECURITY.md). Le minacce considerate e i rischi accettati sono in [docs/threat-model.md](docs/threat-model.md).

---

## Licenza

Copyright (C) 2026 MR

Progetto open-source rilasciato sotto licenza **GNU General Public License v3.0 (GPL-3.0-only)**. Consulta il file [LICENSE](LICENSE) per i dettagli.
