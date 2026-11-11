//! Tesseract as a second recognition engine, for the languages the bundled ocrs models cannot
//! read: anything with accents or another alphabet (Finnish "ä ö å", Swedish, German, …).
//!
//! Tesseract (Apache-2.0) runs as a separate program the user installed, never linked: PdfCraft
//! writes the page image to a private temporary folder, runs `tesseract … tsv`, and reads the
//! word boxes back. Nothing goes over the network. Its trained language data (`fin.traineddata`
//! and so on) comes with the user's Tesseract installation, not with PdfCraft.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Line, OcrError, Recognize, Word};

/// How long one page may take before Tesseract is stopped.
pub const PAGE_LIMIT: Duration = Duration::from_secs(300);
/// How long listing the installed languages may take.
const LIST_LIMIT: Duration = Duration::from_secs(20);
/// The most bytes of Tesseract output read (a page's TSV is a few hundred kilobytes).
const OUTPUT_CAP: u64 = 32 * 1024 * 1024;
/// The most languages one recognition combines.
pub const MAX_LANGUAGES: usize = 8;

/// ISO 639-1 codes the UI offers and the Tesseract language each one needs.
pub const LANGUAGE_CODES: &[(&str, &str, &str)] = &[
    ("fi", "fin", "Finnish"),
    ("sv", "swe", "Swedish"),
    ("en", "eng", "English"),
    ("de", "deu", "German"),
    ("fr", "fra", "French"),
    ("es", "spa", "Spanish"),
    ("it", "ita", "Italian"),
    ("nl", "nld", "Dutch"),
    ("no", "nor", "Norwegian"),
    ("da", "dan", "Danish"),
    ("et", "est", "Estonian"),
    ("pl", "pol", "Polish"),
    ("pt", "por", "Portuguese"),
    ("ru", "rus", "Russian"),
];

/// The Tesseract language for an ISO 639-1 code, or a Tesseract code given as is (`fin`).
pub fn tesseract_language(code: &str) -> Option<&'static str> {
    let code = code.trim();
    LANGUAGE_CODES.iter().find(|(iso, tess, _)| code.eq_ignore_ascii_case(iso) || code.eq_ignore_ascii_case(tess)).map(|(_, t, _)| *t)
}

/// A Tesseract program, and the folder of language data to use instead of its own (`None`: its
/// own `tessdata`).
#[derive(Clone, Debug, PartialEq)]
pub struct Tesseract {
    pub exe: PathBuf,
    pub tessdata: Option<PathBuf>,
}

/// Language data PdfCraft brings itself: `$PDFCRAFT_TESSDATA`, else a `tessdata` folder beside
/// the PdfCraft executable that holds at least one `.traineddata` file. A packaged PdfCraft can
/// so carry Finnish and Swedish without touching the system Tesseract's own folder.
pub fn tessdata_dir() -> Option<PathBuf> {
    let has_data =
        |d: &Path| std::fs::read_dir(d).is_ok_and(|mut it| it.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "traineddata"))));
    if let Some(d) = std::env::var_os("PDFCRAFT_TESSDATA").map(PathBuf::from) {
        return has_data(&d).then_some(d);
    }
    let beside = std::env::current_exe().ok()?.parent()?.join("tessdata");
    has_data(&beside).then_some(beside)
}

impl Tesseract {
    /// The first Tesseract found: `$PDFCRAFT_TESSERACT`, then `tesseract` on the `PATH`, then
    /// the usual install locations.
    pub fn find() -> Option<Tesseract> {
        candidates().into_iter().find(|p| p.is_file()).map(|exe| Tesseract { exe, tessdata: tessdata_dir() })
    }

    /// The program, pointed at PdfCraft's own language data when there is some.
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        no_console(&mut cmd);
        if let Some(dir) = &self.tessdata {
            cmd.arg("--tessdata-dir").arg(dir);
        }
        cmd
    }

    /// The languages this installation can read (Tesseract codes such as `fin`), sorted.
    pub fn languages(&self) -> Result<Vec<String>, OcrError> {
        let dir = Scratch::new()?;
        let out = dir.path().join("langs.txt");
        let mut cmd = self.command();
        cmd.arg("--list-langs");
        run(&mut cmd, Some(&out), LIST_LIMIT)?;
        let text = read_capped(&out)?;
        let mut langs: Vec<String> = text
            .lines()
            .skip_while(|l| !l.contains("languages"))
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .filter(|l| *l != "osd")
            .map(str::to_string)
            .collect();
        langs.sort();
        langs.dedup();
        Ok(langs)
    }

    /// Recognise the words in an RGBA, RGB or grey image with the given Tesseract languages
    /// (`["fin", "eng"]`). The result has the same shape as the ocrs engine's.
    pub fn recognize(&self, pixels: &[u8], width: u32, height: u32, languages: &[String]) -> Result<Vec<Line>, OcrError> {
        if width == 0 || height == 0 || pixels.is_empty() {
            return Err(OcrError::EmptyImage);
        }
        let langs: Vec<&str> = languages.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        if langs.is_empty() || langs.len() > MAX_LANGUAGES || langs.iter().any(|l| !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            return Err(OcrError::Recognize(format!("choose 1 to {MAX_LANGUAGES} languages by their Tesseract codes (fin, swe, eng, …)")));
        }
        let ppm = to_ppm(pixels, width, height)?;
        let dir = Scratch::new()?;
        let image = dir.path().join("page.ppm");
        std::fs::File::create(&image).and_then(|mut f| f.write_all(&ppm)).map_err(io_err)?;
        let base = dir.path().join("words");
        let mut cmd = self.command();
        // --psm 3: automatic page segmentation (Tesseract's default), stated so a user config
        // file cannot change it. One thread: pages are already recognised one at a time.
        // The TSV output is asked for as a variable, not by the `tsv` config file's name: config
        // files live in the language folder, and a folder of our own has only language data.
        cmd.arg(&image).arg(&base).args(["-l", &langs.join("+"), "--psm", "3", "-c", "tessedit_create_tsv=1"]).env("OMP_THREAD_LIMIT", "1");
        run(&mut cmd, None, PAGE_LIMIT)?;
        let tsv = read_capped(&base.with_extension("tsv"))?;
        Ok(parse_tsv(&tsv))
    }
}

/// A Tesseract with the languages to read, ready to recognise pages.
#[derive(Clone, Debug, PartialEq)]
pub struct TesseractRun {
    pub tesseract: Tesseract,
    pub languages: Vec<String>,
}

impl Recognize for TesseractRun {
    fn recognize(&self, pixels: &[u8], width: u32, height: u32) -> Result<Vec<Line>, OcrError> {
        self.tesseract.recognize(pixels, width, height, &self.languages)
    }
}

/// Tesseract is a console program: started from the windowed app on Windows it would flash a
/// console window for every page. CREATE_NO_WINDOW starts it without one.
fn no_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Where Tesseract may be, in the order [`Tesseract::find`] tries them.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("PDFCRAFT_TESSERACT") {
        out.push(PathBuf::from(p));
    }
    let exe = if cfg!(windows) { "tesseract.exe" } else { "tesseract" };
    if let Some(path) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&path).map(|d| d.join(exe)));
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(d) = std::env::var_os(var) {
                out.push(PathBuf::from(&d).join("Tesseract-OCR").join(exe));
                out.push(PathBuf::from(d).join("Programs").join("Tesseract-OCR").join(exe));
            }
        }
    } else {
        out.extend(["/opt/homebrew/bin/tesseract", "/usr/local/bin/tesseract", "/usr/bin/tesseract"].map(PathBuf::from));
    }
    out
}

/// Words from Tesseract's TSV output, grouped into lines in reading order. Rows are
/// `level page block par line word left top width height conf text`; words are level 5.
pub fn parse_tsv(tsv: &str) -> Vec<Line> {
    let mut lines: Vec<((u32, u32, u32), Line)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.splitn(12, '\t').collect();
        let [level, _page, block, par, line, _word, left, top, width, height, conf, text] = cols.as_slice() else { continue };
        let num = |s: &str| s.trim().parse::<u32>().ok();
        let (Some(5), Some(block), Some(par), Some(line)) = (num(level), num(block), num(par), num(line)) else { continue };
        let text = text.trim();
        // Tesseract writes -1 for rows without a word and low confidences for noise.
        let conf = conf.trim().parse::<f32>().unwrap_or(-1.0);
        if text.is_empty() || conf < 0.0 {
            continue;
        }
        let (Some(l), Some(t), Some(w), Some(h)) = (num(left), num(top), num(width), num(height)) else { continue };
        if w == 0 || h == 0 {
            continue;
        }
        let word = Word { text: text.to_string(), rect: [l as f32, t as f32, l.saturating_add(w) as f32, t.saturating_add(h) as f32] };
        let key = (block, par, line);
        match lines.last_mut() {
            Some((k, current)) if *k == key => current.words.push(word),
            _ => lines.push((key, Line { words: vec![word] })),
        }
    }
    lines.into_iter().map(|(_, l)| l).collect()
}

/// A binary PPM (P6) of an RGBA, RGB or grey image; Tesseract reads it without extra libraries.
fn to_ppm(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, OcrError> {
    let n = (width as usize).checked_mul(height as usize).ok_or(OcrError::EmptyImage)?;
    let channels = if n.checked_mul(4) == Some(pixels.len()) {
        4
    } else if n.checked_mul(3) == Some(pixels.len()) {
        3
    } else if n == pixels.len() {
        1
    } else {
        return Err(OcrError::Recognize(format!("{} bytes is no {width}×{height} image", pixels.len())));
    };
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.reserve(n.saturating_mul(3));
    for px in pixels.chunks_exact(channels) {
        match px {
            [g] => out.extend_from_slice(&[*g, *g, *g]),
            [r, g, b] | [r, g, b, _] => out.extend_from_slice(&[*r, *g, *b]),
            _ => {}
        }
    }
    Ok(out)
}

fn io_err(e: std::io::Error) -> OcrError {
    OcrError::Recognize(e.to_string())
}

fn read_capped(path: &Path) -> Result<String, OcrError> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| OcrError::Recognize(format!("Tesseract wrote no result ({e})")))?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_CAP).read_to_end(&mut bytes).map_err(io_err)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Run `cmd` with no input, its standard output to `stdout` (or nowhere), stopping it after
/// `limit`. Fails when it cannot start, times out or exits unsuccessfully.
fn run(cmd: &mut Command, stdout: Option<&Path>, limit: Duration) -> Result<(), OcrError> {
    let out = match stdout {
        Some(p) => Stdio::from(std::fs::File::create(p).map_err(io_err)?),
        None => Stdio::null(),
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| OcrError::Recognize(format!("Tesseract could not be started ({e})")))?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(OcrError::Recognize(format!("Tesseract stopped with {status}; check that the chosen languages are installed")));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OcrError::Recognize(format!("Tesseract did not finish within {} s", limit.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io_err(e));
            }
        }
    }
}

/// A private temporary folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, OcrError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let name = format!("pdfcraft-ocr-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        // `create_dir` (not `_all`) fails if the name exists, so a planted folder is never used.
        std::fs::create_dir(&path).map_err(io_err)?;
        Ok(Scratch(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t\n\
4\t1\t1\t1\t1\t0\t10\t20\t300\t30\t-1\t\n\
5\t1\t1\t1\t1\t1\t10\t20\t120\t30\t96.5\tPäivämäärä\n\
5\t1\t1\t1\t1\t2\t140\t22\t60\t28\t91.0\t1.12.\n\
5\t1\t1\t1\t1\t3\t210\t22\t10\t28\t-1\t \n\
5\t1\t1\t1\t2\t1\t10\t60\t80\t30\t88\tÅbo\n\
5\t1\t2\t1\t1\t1\t10\t200\t0\t30\t90\tzero\n\
5\t1\t2\t1\t1\t2\tx\t200\t40\t30\t90\tbad\n\
5\t1\t2\t1\t1\t3\t60\t200\t40\t30\t90\tTäs\tsä\n\
garbage\n";

    #[test]
    fn tsv_words_become_lines_with_their_boxes() {
        let lines = parse_tsv(TSV);
        let texts: Vec<String> = lines.iter().map(Line::text).collect();
        assert_eq!(texts, ["Päivämäärä 1.12.", "Åbo", "Täs\tsä"]);
        assert_eq!(lines[0].words[0].rect, [10.0, 20.0, 130.0, 50.0]);
        assert!(parse_tsv("").is_empty());
    }

    #[test]
    fn images_become_ppm_in_any_channel_count() {
        assert_eq!(to_ppm(&[1, 2, 3, 255, 4, 5, 6, 255], 2, 1).unwrap(), b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06");
        assert_eq!(to_ppm(&[1, 2, 3], 1, 1).unwrap(), b"P6\n1 1\n255\n\x01\x02\x03");
        assert_eq!(to_ppm(&[9], 1, 1).unwrap(), b"P6\n1 1\n255\n\x09\x09\x09");
        assert!(to_ppm(&[1, 2], 1, 1).is_err());
        assert!(to_ppm(&[1], u32::MAX, u32::MAX).is_err());
    }

    /// A language folder of PdfCraft's own replaces the installation's: a real Tesseract lists
    /// what is in it. Skipped without Tesseract.
    #[test]
    fn own_language_data_is_used_when_given() {
        let Some(mut t) = Tesseract::find() else { return };
        let Ok(system) = t.languages() else { return };
        let Some(lang) = system.first().cloned() else { return };
        let Some(src) = ["/usr/share/tesseract-ocr/5/tessdata", "/usr/share/tesseract-ocr/4.00/tessdata", "/opt/homebrew/share/tessdata"]
            .iter()
            .map(|d| Path::new(d).join(format!("{lang}.traineddata")))
            .find(|p| p.is_file())
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&src, dir.path().join("only.traineddata")).unwrap();
        t.tessdata = Some(dir.path().to_path_buf());
        assert_eq!(t.languages().unwrap(), ["only"]);
    }

    /// A language folder with nothing but language data (no `configs/tsv`) still gives words:
    /// this is how a packaged PdfCraft carries Finnish. Skipped without Tesseract.
    #[test]
    fn a_folder_of_language_data_alone_is_enough_to_read_a_page() {
        let Some(mut t) = Tesseract::find() else { return };
        let Some(src) = ["/usr/share/tesseract-ocr/5/tessdata", "/usr/share/tesseract-ocr/4.00/tessdata", "/opt/homebrew/share/tessdata"]
            .iter()
            .map(|d| Path::new(d).join("eng.traineddata"))
            .find(|p| p.is_file())
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&src, dir.path().join("eng.traineddata")).unwrap();
        t.tessdata = Some(dir.path().to_path_buf());
        // A white page: no words, but Tesseract must have written its TSV for this to be Ok.
        let (w, h) = (200u32, 80u32);
        let white = vec![255u8; (w * h) as usize];
        assert_eq!(t.recognize(&white, w, h, &["eng".to_string()]).map(|l| l.len()).map_err(|e| e.to_string()), Ok(0));
    }

    #[test]
    fn languages_are_named_by_iso_or_tesseract_codes() {
        assert_eq!(tesseract_language("fi"), Some("fin"));
        assert_eq!(tesseract_language("SWE"), Some("swe"));
        assert_eq!(tesseract_language("xx"), None);
    }

    #[test]
    fn bad_language_lists_are_refused_before_running() {
        let t = Tesseract { exe: PathBuf::from("/nonexistent/tesseract"), tessdata: None };
        let px = [255u8; 4];
        for langs in [vec![], vec!["fin; rm -rf".to_string()], vec!["eng".to_string(); MAX_LANGUAGES + 1]] {
            assert!(matches!(t.recognize(&px, 1, 1, &langs), Err(OcrError::Recognize(m)) if m.contains("languages")), "{langs:?}");
        }
        assert!(matches!(t.recognize(&px, 1, 1, &["eng".to_string()]), Err(OcrError::Recognize(m)) if m.contains("could not be started")));
    }
}
