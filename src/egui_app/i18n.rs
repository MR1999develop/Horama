//! Translations of the egui interface.
//!
//! Texts are written in English in the code and passed through [`tr`] (fixed text) or [`trf`]
//! (text with `{}` placeholders, filled in order). The Italian catalog is below; a text missing
//! from it is shown in English. The language is the system one unless the user chose another
//! (View > Language). A test checks that every text passed to `tr`/`trf` in the interface has
//! its translation.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    English,
    Italian,
}

/// Language choices of the View menu: (setting value, name in its own language).
pub const CHOICES: [(&str, &str); 3] = [
    ("", "System language"),
    ("en", "English"),
    ("it", "Italiano"),
];

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Applies the language setting: "" = the system language, otherwise "en" or "it".
pub fn set_language(setting: &str) {
    let lang = match setting {
        "it" => Lang::Italian,
        "en" => Lang::English,
        _ => system_language(),
    };
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Italian,
        _ => Lang::English,
    }
}

fn system_language() -> Lang {
    match sys_locale::get_locale() {
        Some(l) if l.to_lowercase().starts_with("it") => Lang::Italian,
        _ => Lang::English,
    }
}

fn italian() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| IT.iter().copied().collect())
}

/// The text in the current language.
pub fn tr(en: &str) -> &str {
    match current() {
        Lang::English => en,
        Lang::Italian => italian().get(en).copied().unwrap_or(en),
    }
}

/// The text in the current language, with each `{}` replaced by the next argument.
pub fn trf(en: &str, args: &[&dyn Display]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut parts = tr(en).split("{}");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        out.push_str(part);
    }
    out
}

/// English → Italian.
const IT: &[(&str, &str)] = &[
    // Window and menus
    (
        "Open a folder to view images",
        "Apri una cartella per vedere le immagini",
    ),
    ("Open Folder…", "Apri cartella…"),
    ("Recent Folders", "Cartelle recenti"),
    ("Clear List", "Svuota elenco"),
    ("Save a Copy As…", "Salva una copia come…"),
    ("Rename…", "Rinomina…"),
    ("Move to Recycle Bin", "Sposta nel Cestino"),
    ("Exit", "Esci"),
    ("Image", "Immagine"),
    ("Rotate Right", "Ruota a destra"),
    ("Rotate Left", "Ruota a sinistra"),
    ("View", "Visualizza"),
    ("Image Info", "Informazioni immagine"),
    ("Histogram and metadata (I)", "Istogramma e metadati (I)"),
    ("Fit to Window", "Adatta alla finestra"),
    ("Actual Size (100%)", "Dimensioni reali (100%)"),
    ("Zoom In", "Ingrandisci"),
    ("Zoom Out", "Riduci"),
    ("Theme", "Tema"),
    ("System", "Sistema"),
    ("Light", "Chiaro"),
    ("Dark", "Scuro"),
    ("Language", "Lingua"),
    ("System language", "Lingua di sistema"),
    // Browsing
    (
        "'{}' is not an image format Horama can open.",
        "'{}' non è un formato d'immagine che Horama sa aprire.",
    ),
    ("'{}' does not exist.", "'{}' non esiste."),
    ("Scanning {}…", "Lettura di {}…"),
    (
        "Could not scan the folder: {}",
        "Impossibile leggere la cartella: {}",
    ),
    (
        "'{}' was not found in its folder.",
        "'{}' non è stato trovato nella sua cartella.",
    ),
    (
        "No supported images in this folder.",
        "Nessuna immagine supportata in questa cartella.",
    ),
    ("Cannot display {}: {}", "Impossibile mostrare {}: {}"),
    ("Nothing to undo.", "Niente da annullare."),
    ("No folder open", "Nessuna cartella aperta"),
    ("Search file names", "Cerca nei nomi dei file"),
    ("{} images shown", "{} immagini mostrate"),
    (" — loading {}…", " — caricamento di {}…"),
    ("Loading…", "Caricamento…"),
    ("Collapse", "Comprimi"),
    ("Expand", "Espandi"),
    ("Thumbnail of {}", "Miniatura di {}"),
    ("Thumbnail of {} ({})", "Miniatura di {} ({})"),
    ("Folder {}", "Cartella {}"),
    ("Image view", "Vista dell'immagine"),
    // Info panel
    ("No image", "Nessuna immagine"),
    ("Histogram", "Istogramma"),
    ("File Name", "Nome file"),
    ("Folder", "Cartella"),
    ("Format", "Formato"),
    ("Dimensions", "Dimensioni"),
    ("Megapixels", "Megapixel"),
    ("Bit Depth", "Profondità"),
    ("File Size", "Dimensione file"),
    ("Embedded ICC", "ICC incorporato"),
    ("ICC (ignored)", "ICC (ignorato)"),
    ("Camera Make", "Marca fotocamera"),
    ("Camera Model", "Modello fotocamera"),
    ("Lens", "Obiettivo"),
    ("Date Taken", "Data di scatto"),
    ("Shutter Speed", "Tempo di posa"),
    ("Aperture", "Diaframma"),
    ("ISO", "ISO"),
    ("Focal Length", "Lunghezza focale"),
    ("Focal Length (35mm eq)", "Lunghezza focale (equiv. 35 mm)"),
    ("Exposure Bias", "Compensazione esposizione"),
    ("Exposure Program", "Programma di esposizione"),
    ("White Balance", "Bilanciamento del bianco"),
    ("Metering Mode", "Modalità di misurazione"),
    ("Flash", "Flash"),
    ("Software", "Software"),
    // Filters and ratings
    ("All", "Tutti"),
    ("Other", "Altro"),
    ("Without embedded profile", "Senza profilo incorporato"),
    (
        "Reading the size of {} image(s)…",
        "Lettura delle dimensioni di {} immagini…",
    ),
    (
        "Could not read the image sizes: {}",
        "Impossibile leggere le dimensioni delle immagini: {}",
    ),
    ("{}: {} star(s)", "{}: {} stelle"),
    ("{} images: {} star(s)", "{} immagini: {} stelle"),
    ("Filters", "Filtri"),
    ("Rating (stars)", "Valutazione (stelle)"),
    ("Unrated", "Senza voto"),
    ("Aspect ratio", "Proporzioni"),
    ("Color profile", "Profilo colore"),
    ("Clear filters", "Azzera filtri"),
    (
        "The profile is known only for images already opened.",
        "Il profilo è noto solo per le immagini già aperte.",
    ),
    ("Rating (keys 0–5)", "Valutazione (tasti 0–5)"),
    ("Rating", "Valutazione"),
    // File operations
    ("Rotated: {}", "Ruotata: {}"),
    (
        "Rotation not saved for '{}': {}",
        "Rotazione non salvata per '{}': {}",
    ),
    (
        "'{}' moved to the Recycle Bin.",
        "'{}' spostato nel Cestino.",
    ),
    (
        "{} files moved to the Recycle Bin.",
        "{} file spostati nel Cestino.",
    ),
    ("Restored {} file(s).", "{} file ripristinati."),
    (
        "{} file(s) could not be moved to the Recycle Bin and are back in the list. {}",
        "{} file non sono stati spostati nel Cestino e sono tornati nell'elenco. {}",
    ),
    ("Rename", "Rinomina"),
    ("Rename {} files", "Rinomina {} file"),
    (
        "The files become \"Name (1)\", \"Name (2)\"… keeping their extension.",
        "I file diventano \"Nome (1)\", \"Nome (2)\"… e mantengono l'estensione.",
    ),
    ("New name", "Nuovo nome"),
    ("Cancel", "Annulla"),
    ("Not renamed: {}", "Non rinominato: {}"),
    ("Renamed to {}", "Rinominato in {}"),
    ("{} files renamed", "{} file rinominati"),
    ("Save a Copy As", "Salva una copia come"),
    (
        "Choose another name: the original is never overwritten.",
        "Scegli un altro nome: l'originale non viene mai sovrascritto.",
    ),
    ("JPEG Quality", "Qualità JPEG"),
    (
        "Same compression as the original",
        "Stessa compressione dell'originale",
    ),
    (
        "Reuses the tables of the original: repeated saves do not wear the image, and an unchanged image is copied without recompression.",
        "Riusa le tabelle dell'originale: i salvataggi ripetuti non consumano l'immagine e un'immagine non modificata viene copiata senza ricompressione.",
    ),
    ("Quality", "Qualità"),
    (
        "From 90 the color keeps full resolution (4:4:4).",
        "Da 90 il colore mantiene la piena risoluzione (4:4:4).",
    ),
    ("Save", "Salva"),
    ("Saving {}…", "Salvataggio di {}…"),
    ("Saved '{}'.", "'{}' salvato."),
    ("'{}' not saved: {}", "'{}' non salvato: {}"),
    ("Undo", "Annulla"),
    ("Close", "Chiudi"),
    // Color profiles
    ("Color Profile", "Profilo colore"),
    ("Color profile: {}", "Profilo colore: {}"),
    ("Embedded Profile", "Profilo incorporato"),
    (
        "Load Custom ICC/ICM Profile",
        "Carica un profilo ICC/ICM personalizzato",
    ),
    (
        "Load Custom ICC/ICM Profile…",
        "Carica un profilo ICC/ICM personalizzato…",
    ),
    (
        "'{}' is not a valid RGB ICC profile.",
        "'{}' non è un profilo ICC RGB valido.",
    ),
    (
        "Images with an embedded profile are converted; the others get it assigned.",
        "Le immagini con un profilo incorporato vengono convertite, alle altre viene assegnato.",
    ),
    // Crop and history
    ("Edit", "Modifica"),
    ("Redo", "Ripeti"),
    ("Nothing to redo.", "Niente da ripetere."),
    ("Crop", "Ritaglia"),
    ("Free", "Libero"),
    ("Apply", "Applica"),
    ("Save Crop", "Salva ritaglio"),
    (
        "Nothing to save: only cropped images are saved.",
        "Niente da salvare: si salvano solo le immagini ritagliate.",
    ),
    (
        "Not rotated: {} (save or undo the crop first).",
        "Non ruotate: {} (prima salva o annulla il ritaglio).",
    ),
    ("Unsaved crops", "Ritagli non salvati"),
    (
        "{} cropped image(s) are not saved. Save them as new files next to the originals?",
        "{} immagini ritagliate non sono salvate. Salvarle come nuovi file accanto agli originali?",
    ),
    ("Save and Close", "Salva e chiudi"),
    ("Close Without Saving", "Chiudi senza salvare"),
    (
        "{} selected, the first {} shown",
        "{} selezionate, mostrate le prime {}",
    ),
    // Keyboard
    ("Open File…", "Apri file…"),
    ("Open File", "Apri file"),
    ("Images", "Immagini"),
    ("Open Folder", "Apri cartella"),
    ("Refresh Folder", "Aggiorna cartella"),
    ("Next Image", "Immagine successiva"),
    ("Previous Image", "Immagine precedente"),
    ("10 Images Forward", "10 immagini avanti"),
    ("10 Images Back", "10 immagini indietro"),
    ("Next Color Profile", "Profilo colore successivo"),
    ("Keyboard Shortcuts", "Scorciatoie da tastiera"),
    ("Keyboard Shortcuts…", "Scorciatoie da tastiera…"),
    (
        "Press the new keys… (Esc cancels)",
        "Premi i nuovi tasti… (Esc annulla)",
    ),
    ("Change", "Cambia"),
    (
        "Fixed keys: 0–5 rating, F fit, I info panel, Ctrl+Shift+S save a copy, F6 next panel.",
        "Tasti fissi: 0–5 valutazione, F adatta, I pannello informazioni, Ctrl+Maiusc+S salva una copia, F6 pannello successivo.",
    ),
    ("Reset to Defaults", "Ripristina i predefiniti"),
    ("{} is already used by \"{}\".", "{} è già usato da \"{}\"."),
    ("Next Panel", "Pannello successivo"),
    // Tools
    ("Pixel", "Pixel"),
    ("Display profile: {}", "Profilo del monitor: {}"),
    (
        "None of the dropped files is an image format Horama can open.",
        "Nessuno dei file trascinati è un formato d'immagine che Horama sa aprire.",
    ),
    ("{} selected files", "{} file selezionati"),
    (
        "Rotation shown only: {} (this format is never modified).",
        "Rotazione solo a schermo: {} (questo formato non viene mai modificato).",
    ),
    ("Open With", "Apri con"),
    ("Choose Another App…", "Scegli un'altra app…"),
    ("'{}' not opened: {}", "'{}' non aperto: {}"),
    // Windows, toolbar and menus
    ("Remove from the list", "Togli dall'elenco"),
    ("Preferences…", "Preferenze…"),
    ("Preferences", "Preferenze"),
    ("Help", "Aiuto"),
    ("About {}", "Informazioni su {}"),
    ("Version {}", "Versione {}"),
    (
        "Color-managed RAW and image viewer.",
        "Visualizzatore di immagini e RAW con gestione del colore.",
    ),
    (
        "Free software under the GNU General Public License v3.0 only (GPL-3.0-only), without any warranty.",
        "Software libero con licenza GNU General Public License v3.0 only (GPL-3.0-only), senza alcuna garanzia.",
    ),
    (
        "Settings, ratings and shortcuts are saved as you change them.",
        "Impostazioni, valutazioni e scorciatoie si salvano mentre le cambi.",
    ),
    ("[This folder]", "[Questa cartella]"),
    ("Add", "Aggiungi"),
    (
        "Save this aspect ratio among the filters",
        "Salva queste proporzioni tra i filtri",
    ),
    ("Remove", "Togli"),
    (
        "Remove this aspect ratio from the filters",
        "Togli queste proporzioni dai filtri",
    ),
    ("Show in Folder", "Mostra nella cartella"),
    ("{} star(s)", "{} stelle"),
    ("Zoom", "Zoom"),
    (
        "Keep the zoom when changing image",
        "Mantieni lo zoom cambiando immagine",
    ),
    (
        "Keep this profile for the other images",
        "Usa questo profilo anche per le altre immagini",
    ),
    // Metadata values written by the library
    ("None (Untagged)", "Nessuno (senza profilo)"),
    (
        "8-bit per channel (embedded JPEG preview)",
        "8 bit per canale (anteprima JPEG incorporata)",
    ),
    (
        "{}-bit per channel (shown at 8-bit)",
        "{} bit per canale (mostrata a 8 bit)",
    ),
    (
        "{}-bit sensor data (shown at 8-bit)",
        "Dati del sensore a {} bit (mostrati a 8 bit)",
    ),
    ("{}-bit per channel", "{} bit per canale"),
    (
        "Open a folder or an image (File menu), or drop one here",
        "Apri una cartella o un'immagine (menu File), o trascinala qui",
    ),
    // Files changed by other programs.
    ("New", "Nuovo"),
    ("Changed", "Modificato"),
    ("Renamed", "Rinominato"),
    ("New file", "File nuovo"),
    (
        "Changed on disk by another program",
        "Modificato sul disco da un altro programma",
    ),
    (
        "Renamed by another program",
        "Rinominato da un altro programma",
    ),
    (
        "{} was deleted by another program.",
        "{} è stato eliminato da un altro programma.",
    ),
    // Status bar, panels, zoom.
    (
        "Decoded images kept in memory for instant browsing",
        "Immagini decodificate tenute in memoria per scorrere senza attese",
    ),
    (
        "RAM cache: {} / {} ({} / {} MB)",
        "Cache RAM: {} / {} ({} / {} MB)",
    ),
    ("Folders", "Cartelle"),
    (
        "Folder tree, search and filters",
        "Albero delle cartelle, ricerca e filtri",
    ),
    ("Zoom Levels", "Livelli di zoom"),
    ("Rating of {}", "Valutazione di {}"),
    // Color section of the info panel.
    ("Color Management", "Gestione colore"),
    ("Metadata", "Metadati"),
    ("e.g. 16:10", "es. 16:10"),
    // Overwrite Original.
    ("Overwrite Original…", "Sovrascrivi l'originale…"),
    (
        "Writes the image as shown over its own file (not for RAW files)",
        "Scrive l'immagine com'è a schermo sul suo file (non per i RAW)",
    ),
    (
        "RAW files are never overwritten: save a copy instead.",
        "I file RAW non si sovrascrivono mai: salvane una copia.",
    ),
    (
        "Horama cannot write this format: save a copy instead.",
        "Horama non sa scrivere questo formato: salvane una copia.",
    ),
    (
        "Nothing to save: crop, rotate or change the color profile first.",
        "Niente da salvare: prima ritaglia, ruota o cambia il profilo colore.",
    ),
    ("Overwrite the original?", "Sovrascrivere l'originale?"),
    (
        "'{}' will be replaced by the image as shown, with its crop, rotation and color profile. The original cannot be recovered.",
        "'{}' sarà sostituito dall'immagine com'è a schermo, con ritaglio, rotazione e profilo colore. L'originale non si potrà recuperare.",
    ),
    (
        "JPEG: compressed like the original, to lose as little as possible.",
        "JPEG: compresso come l'originale, per perdere il meno possibile.",
    ),
    (
        "This file has more than 8 bits per channel or a profile that is not RGB: it will be saved as 8-bit RGB.",
        "Questo file ha più di 8 bit per canale o un profilo non RGB: sarà salvato come RGB a 8 bit.",
    ),
    ("Overwrite", "Sovrascrivi"),
    ("Save a Copy Instead…", "Salva invece una copia…"),
    ("'{}' overwritten.", "'{}' sovrascritto."),
    ("Embedded profile", "Profilo incorporato"),
    ("Profile in use", "Profilo in uso"),
    ("Monitor", "Monitor"),
    ("None ({} assumed)", "Nessuno (si presume {})"),
    (
        "Converted: same look on screen; pixel values and histogram change.",
        "Convertito: a schermo appare uguale; cambiano i valori dei pixel e l'istogramma.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Texts passed to `tr("…")` / `trf("…"` in the interface sources.
    fn texts_in(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        for call in ["tr(\"", "trf(\""] {
            let mut rest = source;
            while let Some(i) = rest.find(call) {
                // `ctr(` or `attr(` are not calls of ours.
                let before = rest[..i].chars().last();
                rest = &rest[i + call.len()..];
                if before
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut text = String::new();
                let mut chars = rest.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                text.push(n);
                            }
                        }
                        '"' => break,
                        c => text.push(c),
                    }
                }
                out.push(text);
            }
        }
        out
    }

    #[test]
    fn every_interface_text_has_its_translation() {
        let sources = [
            include_str!("mod.rs"),
            include_str!("filters.rs"),
            include_str!("files.rs"),
            include_str!("profiles.rs"),
            include_str!("watch.rs"),
            include_str!("edit.rs"),
            include_str!("compare.rs"),
            include_str!("shortcuts.rs"),
            include_str!("tools.rs"),
            include_str!("toolbar.rs"),
            include_str!("prefs.rs"),
            include_str!("display.rs"),
            include_str!("view_rotation.rs"),
            include_str!("overwrite.rs"),
        ];
        let missing: Vec<String> = sources
            .iter()
            .flat_map(|s| texts_in(s))
            .filter(|t| !italian().contains_key(t.as_str()))
            .collect();
        assert!(missing.is_empty(), "no Italian text for: {:?}", missing);
    }

    #[test]
    fn catalog_has_no_duplicates_and_keeps_placeholders() {
        let mut seen = HashSet::new();
        for (en, it) in IT {
            assert!(seen.insert(en), "twice in the catalog: {}", en);
            assert_eq!(en.matches("{}").count(), it.matches("{}").count(), "{}", en);
        }
    }

    #[test]
    fn placeholders_are_filled_in_order() {
        CURRENT.store(Lang::Italian as u8, Ordering::Relaxed);
        assert_eq!(
            trf("Cannot display {}: {}", &[&"a.jpg", &"bad"]),
            "Impossibile mostrare a.jpg: bad"
        );
        assert_eq!(tr("not in the catalog"), "not in the catalog");
        CURRENT.store(Lang::English as u8, Ordering::Relaxed);
        assert_eq!(trf("{} images shown", &[&3]), "3 images shown");
    }
}
