# ADR 0001 — Precisione della pipeline delle immagini

- **Stato:** proposta (da approvare prima di istogramma ed esportazione nell'interfaccia egui)
- **Data:** 02/11/2026

## Contesto

Oggi ogni immagine diventa RGB a 8 bit subito dopo la decodifica (`DecodedImage::rgb_pixels`,
`to_rgb8()` in `image_loader.rs`). Tutto il resto lavora su quei valori:

- la gestione colore (`qcms`, che accetta solo dati a 8 bit) converte profilo incorporato →
  profilo scelto → monitor;
- istogramma, ritaglio, rotazione ed esportazione partono dagli stessi 8 bit;
- la piramide e le miniature fanno le medie sui valori sRGB codificati.

Conseguenze:

1. I 16 bit di TIFF, PNG e RAW (il RAW passa da `imagepipe`, che lavora in `f32`) vanno persi
   prima della gestione colore. Una conversione tra spazi ampi (ProPhoto → sRGB, Rec.2020 →
   Display P3) applicata a dati già arrotondati può produrre *banding* nei gradienti.
2. L'esportazione in TIFF/PNG è sempre a 8 bit, anche da un sorgente a 16 bit.
3. L'istogramma mostra i dati già arrotondati.
4. L'intento di rendering è fisso (Percettivo) e non c'è la compensazione del punto nero.

Le prossime fasi dell'interfaccia egui (istogramma, esportazione, scelta del profilo) si
costruirebbero sopra questa limitazione: conviene decidere prima.

## Opzioni

| | Memoria per un'immagine da 45 MP | Precisione | Lavoro |
|---|---|---|---|
| **A. Restare a 8 bit** | 135 MB | come oggi | nessuno |
| **B. Documento a 16 bit (`u16`), schermo a 8 bit** | 270 MB documento + 135 MB schermo | conserva i 16 bit dei file; conversioni a 16 bit | medio |
| **C. Documento in virgola mobile lineare (`f32`)** | 540 MB documento + 135 MB schermo | massima, anche per editing futuro | alto |

Per la gestione colore a 16 bit o in virgola mobile `qcms` non basta. Le alternative:

- **`moxcms`** (Rust puro, ICC v2/v4, dati `u8`/`u16`/`f32`, intenti di rendering e
  compensazione del punto nero). È già usato dal crate `image` per la conversione tra spazi
  colore: nessuna libreria C, build invariata su Windows e Linux.
- **`lcms2`** (Little CMS, libreria C): la più collaudata, ma aggiunge una dipendenza C alla
  build e alla release.

## Decisione proposta

**Opzione B con `moxcms`.**

- `DecodedImage` conserva i pixel del documento a 16 bit per canale (`Vec<u16>`); i file a 8
  bit vengono allargati senza perdita (`v * 257`).
- La conversione profilo → monitor produce i pixel a 8 bit solo per lo schermo, con il
  dithering facoltativo; il documento non viene mai arrotondato a 8 bit.
- Intento di rendering (Percettivo, Colorimetrico relativo con o senza compensazione del punto
  nero, Saturazione, Colorimetrico assoluto) esposto come impostazione, default Percettivo.
- Istogramma calcolato sui dati del documento (16 bit, raggruppati in 256 classi).
- Esportazione TIFF/PNG a 16 bit quando il sorgente ha più di 8 bit; JPEG e WebP restano a 8
  bit (limite del formato), con la quantizzazione fatta solo all'uscita.
- Piramide e miniature filtrate in luce lineare (già così per la piramide), partendo dai 16 bit.

L'opzione C non serve finché Horama è un visualizzatore con modifiche geometriche (ritaglio,
rotazione): costa il doppio della memoria di B senza un vantaggio visibile. Si potrà passare a
`f32` se arriveranno regolazioni tonali.

## Conseguenze

- **Memoria:** la cache RAM contiene circa il doppio dei byte per immagine; il budget
  (`CACHE_FREE_RAM_RATIO`) resta lo stesso, quindi le immagini precaricate sono circa la metà.
  I limiti RAW (200 MP, memoria libera) vanno ricalcolati con 6 byte per pixel invece di 3.
- **Prestazioni:** conversione colore e piramide lavorano su dati doppi; va misurato il tempo
  di apertura di un RAW da 45 MP prima e dopo (obiettivo: meno del 20% in più).
- **Codice:** `color_manager.rs` passa da `qcms` a `moxcms`; `image_loader.rs` usa
  `to_rgb16()`; `image_saver.rs` riceve `u16` e scrive TIFF/PNG a 16 bit; i test di
  rotazione e ritaglio lavorano sugli stessi dati.
- **Migrazione:** si fa a passi, ciascuno in un ramo con `check.bat` verde:
  1. `moxcms` al posto di `qcms` a parità di dati (8 bit), confrontando i risultati sulle
     immagini di prova;
  2. documento a 16 bit nella libreria, schermo a 8 bit;
  3. istogramma ed esportazione a 16 bit;
  4. intento di rendering e compensazione del punto nero nelle impostazioni.

## Da verificare prima di approvare

- Che `moxcms` gestisca i profili personalizzati e quelli dei monitor già letti oggi
  (profili v2 con curve parametriche e LUT).
- Differenza di colore (ΔE) tra `qcms` e `moxcms` sulle immagini di prova: deve restare sotto
  1 a parità di intento.
