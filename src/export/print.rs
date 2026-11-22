use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::export::ir::DocBlock;
use crate::export::pdf::{
    export_pdf_to_file_with_options_and_base_dir, PdfHeaderFooterOptions, PdfLayoutOptions,
};

fn cleanup_old_spool_files() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if (name_str.starts_with("markdown_studio_print_") || name_str.starts_with("md_word_print_"))
            && (name_str.ends_with(".pdf") || name_str.ends_with(".ps1"))
            && let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                    && now
                        .duration_since(modified)
                        .is_ok_and(|age| age > Duration::from_secs(3600))
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
    }
}

#[cfg(unix)]
fn cups_paper_name(paper: PaperSize) -> &'static str {
    match paper {
        PaperSize::A4 => "A4",
        PaperSize::Letter => "Letter",
        PaperSize::Legal => "Legal",
        PaperSize::A5 => "A5",
        PaperSize::A3 => "A3",
    }
}

static SPOOL_SEQ: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Supported standard paper sizes for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaperSize {
    #[default]
    A4,
    Letter,
    Legal,
    A5,
    A3,
}

impl PaperSize {
    pub const ALL: [PaperSize; 5] = [
        PaperSize::A4,
        PaperSize::Letter,
        PaperSize::Legal,
        PaperSize::A5,
        PaperSize::A3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaperSize::A4 => "A4 (210 × 297 mm)",
            PaperSize::Letter => "Letter (8.5 × 11 in)",
            PaperSize::Legal => "Legal (8.5 × 14 in)",
            PaperSize::A5 => "A5 (148 × 210 mm)",
            PaperSize::A3 => "A3 (297 × 420 mm)",
        }
    }

    pub fn dimensions_mm(self) -> (f32, f32) {
        match self {
            PaperSize::A4 => (210.0, 297.0),
            PaperSize::Letter => (215.9, 279.4),
            PaperSize::Legal => (215.9, 355.6),
            PaperSize::A5 => (148.0, 210.0),
            PaperSize::A3 => (297.0, 420.0),
        }
    }

    pub fn win_paper_kind(self) -> &'static str {
        match self {
            PaperSize::A4 => "A4",
            PaperSize::Letter => "Letter",
            PaperSize::Legal => "Legal",
            PaperSize::A5 => "A5",
            PaperSize::A3 => "A3",
        }
    }
}

/// Page orientation for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageOrientation {
    #[default]
    Portrait,
    Landscape,
}

/// Duplex / double-sided printing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuplexMode {
    #[default]
    Simplex,
    LongEdge,
    ShortEdge,
}

impl DuplexMode {
    pub fn label(self) -> &'static str {
        match self {
            DuplexMode::Simplex => "One-Sided",
            DuplexMode::LongEdge => "Two-Sided (Long Edge)",
            DuplexMode::ShortEdge => "Two-Sided (Short Edge)",
        }
    }
}

/// Installed printer entry detected from the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
    pub status: String,
}

/// User-selected print settings from the Print dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintJobOptions {
    pub printer_name: Option<String>,
    pub copies: u16,
    pub collate: bool,
    pub paper_size: PaperSize,
    pub orientation: PageOrientation,
    pub duplex: DuplexMode,
    pub color: bool,
    pub all_pages: bool,
    pub page_range: String,
    pub header_footer: PdfHeaderFooterOptions,
}

impl Default for PrintJobOptions {
    fn default() -> Self {
        Self {
            printer_name: None,
            copies: 1,
            collate: true,
            paper_size: PaperSize::A4,
            orientation: PageOrientation::Portrait,
            duplex: DuplexMode::Simplex,
            color: true,
            all_pages: true,
            page_range: String::new(),
            header_footer: PdfHeaderFooterOptions::default(),
        }
    }
}

impl PrintJobOptions {
    pub fn layout_options(&self) -> PdfLayoutOptions {
        let (w, h) = self.paper_size.dimensions_mm();
        let (page_w_mm, page_h_mm) = match self.orientation {
            PageOrientation::Portrait => (w, h),
            PageOrientation::Landscape => (h, w),
        };
        PdfLayoutOptions {
            page_w_mm,
            page_h_mm,
            margin_mm: 20.0,
            header_footer: self.header_footer.clone(),
        }
    }
}

/// Parses a 1-based page range specification (e.g. `"1-3, 5"`) into sorted unique 1-based page numbers.
pub fn parse_page_range(spec: &str, max_pages: Option<usize>) -> Result<Vec<usize>, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("Enter a page range such as 1-3 or 1, 4".to_string());
    }
    let mut pages = Vec::new();
    for token in trimmed.split(',') {
        let part = token.trim();
        if part.is_empty() {
            return Err("Empty segment in page range".to_string());
        }
        if let Some((a_str, b_str)) = part.split_once('-') {
            let a = a_str
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("Invalid page number: '{}'", a_str.trim()))?;
            let b = b_str
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("Invalid page number: '{}'", b_str.trim()))?;
            if a == 0 || b == 0 {
                return Err("Page numbers start at 1".to_string());
            }
            if b < a {
                return Err(format!("Invalid page range '{part}': end < start"));
            }
            if let Some(max_p) = max_pages {
                if a > max_p {
                    continue;
                }
                let end = b.min(max_p);
                for p in a..=end {
                    if !pages.contains(&p) {
                        pages.push(p);
                    }
                }
            } else {
                for p in a..=b.min(a.saturating_add(500)) {
                    if !pages.contains(&p) {
                        pages.push(p);
                    }
                }
            }
        } else {
            let p = part
                .parse::<usize>()
                .map_err(|_| format!("Invalid page number: '{part}'"))?;
            if p == 0 {
                return Err("Page numbers start at 1".to_string());
            }
            if max_pages.is_none_or(|m| p <= m) && !pages.contains(&p) {
                pages.push(p);
            }
        }
    }
    if pages.is_empty() {
        return Err("No matching pages in the specified range".to_string());
    }
    Ok(pages)
}

/// Enumerates printers installed on the host OS, placing the system default printer first.
pub fn list_os_printers() -> Vec<PrinterInfo> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let ps = r#"
Add-Type -AssemblyName System.Drawing
$ps = New-Object System.Drawing.Printing.PrinterSettings
$def = $ps.PrinterName
$statusMap = @{}
try {
    Get-Printer -ErrorAction SilentlyContinue | ForEach-Object {
        $statusMap[$_.Name] = "$($_.PrinterStatus)"
    }
} catch {}
foreach ($name in [System.Drawing.Printing.PrinterSettings]::InstalledPrinters) {
    $isDef = if ($name -eq $def) { '1' } else { '0' }
    $st = if ($statusMap.ContainsKey($name)) { $statusMap[$name] } else { 'Ready' }
    Write-Output "$isDef`t$st`t$name"
}
"#;
        if let Ok(out) = Command::new("powershell")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-NonInteractive", "-Command", ps])
            .output()
            && out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let mut list = Vec::new();
                for line in stdout.lines() {
                    let mut parts = line.splitn(3, '\t');
                    if let (Some(is_def), Some(status), Some(name)) =
                        (parts.next(), parts.next(), parts.next())
                    {
                        let trimmed = name.trim();
                        if !trimmed.is_empty() {
                            list.push(PrinterInfo {
                                name: trimmed.to_string(),
                                is_default: is_def.trim() == "1",
                                status: status.trim().to_string(),
                            });
                        }
                    }
                }
                list.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
                return list;
            }
        Vec::new()
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(out) = Command::new("lpstat").args(["-p", "-d"]).output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut default_name = String::new();
                let mut printers = Vec::new();
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix("system default destination:") {
                        default_name = rest.trim().to_string();
                    } else if let Some(rest) = line.strip_prefix("printer ") {
                        if let Some(name) = rest.split_whitespace().next() {
                            printers.push(PrinterInfo {
                                name: name.to_string(),
                                is_default: false,
                                status: "Ready".to_string(),
                            });
                        }
                    }
                }
                for p in &mut printers {
                    if p.name == default_name {
                        p.is_default = true;
                    }
                }
                printers.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
                return printers;
            }
        }
        Vec::new()
    }
}

/// Generates a paginated print-ready PDF in a temporary spool file using default A4 layout.
pub fn create_print_spool_pdf(
    blocks: &[DocBlock],
    title: &str,
    on_progress: impl FnMut(f32),
) -> Result<PathBuf, String> {
    create_print_spool_pdf_with_options_and_base_dir(
        blocks,
        title,
        &PrintJobOptions::default(),
        None,
        on_progress,
    )
}

/// Generates a paginated print-ready PDF in a temporary spool file matching `options` page geometry.
pub fn create_print_spool_pdf_with_options(
    blocks: &[DocBlock],
    title: &str,
    options: &PrintJobOptions,
    on_progress: impl FnMut(f32),
) -> Result<PathBuf, String> {
    create_print_spool_pdf_with_options_and_base_dir(blocks, title, options, None, on_progress)
}

/// Generates a paginated print-ready PDF in a temporary spool file with explicit geometry and optional image base dir.
pub fn create_print_spool_pdf_with_options_and_base_dir(
    blocks: &[DocBlock],
    title: &str,
    options: &PrintJobOptions,
    base_dir: Option<&Path>,
    on_progress: impl FnMut(f32),
) -> Result<PathBuf, String> {
    cleanup_old_spool_files();
    let seq = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let filename = format!("markdown_studio_print_{pid}_{seq}.pdf");
    let spool_path = std::env::temp_dir().join(filename);
    export_pdf_to_file_with_options_and_base_dir(
        blocks,
        title,
        &spool_path,
        options.layout_options(),
        base_dir,
        on_progress,
    )?;
    Ok(spool_path)
}

/// Dispatches a print-ready PDF spool file to the native OS print spooler using default options.
pub fn dispatch_to_os_printer(pdf_path: &Path) -> Result<String, String> {
    dispatch_to_os_printer_with_options(pdf_path, &PrintJobOptions::default())
}

/// Dispatches a print-ready PDF spool file to the selected printer with explicit copies/paper/orientation options.
pub fn dispatch_to_os_printer_with_options(
    pdf_path: &Path,
    opts: &PrintJobOptions,
) -> Result<String, String> {
    if !pdf_path.exists() {
        return Err(format!("Spool PDF does not exist: {}", pdf_path.display()));
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let seq = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let script_path =
            std::env::temp_dir().join(format!("markdown_studio_print_job_{pid}_{seq}.ps1"));

        let ps_escape = |s: &str| s.replace('\'', "''");
        let printer_literal = opts
            .printer_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("'{}'", ps_escape(s)))
            .unwrap_or_else(|| "''".to_string());
        let copies = opts.copies.clamp(1, 99);
        let collate = if opts.collate { "$true" } else { "$false" };
        let landscape = if opts.orientation == PageOrientation::Landscape {
            "$true"
        } else {
            "$false"
        };
        let color = if opts.color { "$true" } else { "$false" };
        let duplex_str = match opts.duplex {
            DuplexMode::Simplex => "Simplex",
            DuplexMode::LongEdge => "Vertical",
            DuplexMode::ShortEdge => "Horizontal",
        };
        let paper_kind = opts.paper_size.win_paper_kind();
        let page_range_literal = if opts.all_pages {
            "''".to_string()
        } else {
            format!("'{}'", ps_escape(&opts.page_range))
        };
        let pdf_escaped = ps_escape(&pdf_path.to_string_lossy());

        let ps_script = format!(
            r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Runtime.WindowsRuntime

$pdfPath = '{pdf_escaped}'
$printerName = {printer_literal}
$copies = [int16]{copies}
$collate = {collate}
$landscape = {landscape}
$useColor = {color}
$duplexMode = '{duplex_str}'
$paperKindName = '{paper_kind}'
$pageRangeSpec = {page_range_literal}

$asTaskOp = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{
    $_.Name -eq 'AsTask' -and $_.IsGenericMethodDefinition -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq ('IAsyncOperation' + [char]96 + '1')
}}) | Select-Object -First 1

$asTaskAct = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{
    $_.Name -eq 'AsTask' -and -not $_.IsGenericMethodDefinition -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction'
}}) | Select-Object -First 1

function Await-WinRtOp($op, $resultType) {{
    $task = $asTaskOp.MakeGenericMethod($resultType).Invoke($null, @($op))
    $task.Wait()
    return $task.Result
}}

function Await-WinRtAction($act) {{
    $task = $asTaskAct.Invoke($null, @($act))
    $task.Wait()
}}

[Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime] | Out-Null
[Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType=WindowsRuntime] | Out-Null
[Windows.Storage.Streams.InMemoryRandomAccessStream, Windows.Storage.Streams, ContentType=WindowsRuntime] | Out-Null

$fileOp = [Windows.Storage.StorageFile]::GetFileFromPathAsync($pdfPath)
$storageFile = Await-WinRtOp $fileOp ([Windows.Storage.StorageFile])
$pdfOp = [Windows.Data.Pdf.PdfDocument]::LoadFromFileAsync($storageFile)
$pdfDoc = Await-WinRtOp $pdfOp ([Windows.Data.Pdf.PdfDocument])
$totalPages = [int]$pdfDoc.PageCount

$selectedPages = New-Object System.Collections.Generic.List[int]
if ([string]::IsNullOrWhiteSpace($pageRangeSpec)) {{
    for ($i = 0; $i -lt $totalPages; $i++) {{ $selectedPages.Add($i) }}
}} else {{
    foreach ($tok in $pageRangeSpec.Split(',')) {{
        $p = $tok.Trim()
        if ($p -match '^(\d+)\s*-\s*(\d+)$') {{
            $a = [int]$Matches[1]; $b = [int]$Matches[2]
            if ($a -gt $b) {{ $tmp = $a; $a = $b; $b = $tmp }}
            for ($k = $a; $k -le $b; $k++) {{
                if ($k -ge 1 -and $k -le $totalPages) {{
                    $idx = $k - 1
                    if (-not $selectedPages.Contains($idx)) {{ $selectedPages.Add($idx) }}
                }}
            }}
        }} elseif ($p -match '^\d+$') {{
            $k = [int]$p
            if ($k -ge 1 -and $k -le $totalPages) {{
                $idx = $k - 1
                if (-not $selectedPages.Contains($idx)) {{ $selectedPages.Add($idx) }}
            }}
        }}
    }}
}}
if ($selectedPages.Count -eq 0) {{
    throw "No pages matched page range '$pageRangeSpec' (document has $totalPages pages)."
}}

$pd = New-Object System.Drawing.Printing.PrintDocument
$pd.DocumentName = [System.IO.Path]::GetFileNameWithoutExtension($pdfPath)
if (-not [string]::IsNullOrWhiteSpace($printerName)) {{
    $pd.PrinterSettings.PrinterName = $printerName
}}
$pd.PrinterSettings.Copies = $copies
$pd.PrinterSettings.Collate = $collate
if ($pd.PrinterSettings.CanDuplex) {{
    $pd.PrinterSettings.Duplex = [System.Drawing.Printing.Duplex]::$duplexMode
}}
$pd.DefaultPageSettings.Landscape = $landscape
$pd.DefaultPageSettings.Color = $useColor
foreach ($ps in $pd.PrinterSettings.PaperSizes) {{
    if ("$($ps.Kind)" -eq $paperKindName) {{
        $pd.DefaultPageSettings.PaperSize = $ps
        break
    }}
}}

$script:pageCursor = 0
$pd.add_PrintPage({{
    param($sender, $ev)
    $pageIdx = $selectedPages[$script:pageCursor]
    $pdfPage = $pdfDoc.GetPage([uint32]$pageIdx)
    $stream = New-Object Windows.Storage.Streams.InMemoryRandomAccessStream
    $opts = New-Object Windows.Data.Pdf.PdfPageRenderOptions
    $opts.DestinationWidth = [uint32]([math]::Max(1200, $ev.PageBounds.Width * 3))
    $renderAct = $pdfPage.RenderToStreamAsync($stream, $opts)
    Await-WinRtAction $renderAct
    $netStream = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($stream)
    $img = [System.Drawing.Image]::FromStream($netStream)
    try {{
        $ev.Graphics.DrawImage($img, $ev.PageBounds)
    }} finally {{
        $img.Dispose()
        $netStream.Dispose()
        $stream.Dispose()
        $pdfPage.Dispose()
    }}
    $script:pageCursor++
    $ev.HasMorePages = ($script:pageCursor -lt $selectedPages.Count)
}})

$pd.Print()
Write-Output ("Printed " + $selectedPages.Count + " page(s) to " + $pd.PrinterSettings.PrinterName + " (" + $copies + " cop" + ($(if ($copies -eq 1) {{ "y" }} else {{ "ies" }})) + ")")
"#
        );

        let mut winrt_err = String::new();
        let write_ok = std::fs::write(&script_path, ps_script).is_ok();
        if write_ok {
            let output = Command::new("powershell")
                .creation_flags(CREATE_NO_WINDOW)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script_path)
                .output();
            let _ = std::fs::remove_file(&script_path);
            match output {
                Ok(out) if out.status.success() => {
                    let _ = std::fs::remove_file(pdf_path);
                    let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !msg.is_empty() {
                        return Ok(msg);
                    }
                    return Ok(format!(
                        "Printed via {}",
                        opts.printer_name.as_deref().unwrap_or("default printer")
                    ));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    winrt_err = if !stderr.is_empty() { stderr } else { stdout };
                    if !opts.all_pages
                        && (winrt_err.contains("No pages matched") || winrt_err.contains("page range"))
                    {
                        let _ = std::fs::remove_file(pdf_path);
                        return Err(winrt_err);
                    }
                }
                Err(e) => {
                    winrt_err = e.to_string();
                }
            }
        }

        // Fallback if WinRT PDF rendering is unavailable
        let escaped = ps_escape(&pdf_path.to_string_lossy());
        let ps_cmd = format!(
            "Start-Process -FilePath '{}' -Verb Print -ErrorAction Stop",
            escaped
        );
        let status = Command::new("powershell")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
            .status();

        match status {
            Ok(s) if s.success() => Ok(format!(
                "Dispatched print job via Windows spooler ({})",
                pdf_path.display()
            )),
            _ => {
                let fallback_cmd = format!("Start-Process -FilePath '{}'", escaped);
                let fb = Command::new("powershell")
                    .creation_flags(CREATE_NO_WINDOW)
                    .args(["-NoProfile", "-NonInteractive", "-Command", &fallback_cmd])
                    .status()
                    .map_err(|e| format!("Failed to invoke OS print handler: {e} ({winrt_err})"))?;
                if fb.success() {
                    Ok(format!(
                        "Opened print-ready PDF in default viewer ({})",
                        pdf_path.display()
                    ))
                } else if winrt_err.is_empty() {
                    Err("OS print dispatch failed".to_string())
                } else {
                    Err(format!("OS print dispatch failed: {winrt_err}"))
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("lp");
        if let Some(ref p) = opts.printer_name {
            if !p.trim().is_empty() {
                cmd.args(["-d", p]);
            }
        }
        cmd.args(["-n", &opts.copies.clamp(1, 99).to_string()]);
        if !opts.all_pages && !opts.page_range.trim().is_empty() {
            cmd.args(["-P", opts.page_range.trim()]);
        }
        if opts.orientation == PageOrientation::Landscape {
            cmd.args(["-o", "landscape"]);
        }
        cmd.args(["-o", &format!("media={}", cups_paper_name(opts.paper_size))]);
        let sides = match opts.duplex {
            DuplexMode::Simplex => "one-sided",
            DuplexMode::LongEdge => "two-sided-long-edge",
            DuplexMode::ShortEdge => "two-sided-short-edge",
        };
        cmd.args(["-o", &format!("sides={sides}")]);
        if !opts.color {
            cmd.args(["-o", "ColorModel=Gray"]);
        }
        cmd.args(["-o", if opts.collate { "Collate=True" } else { "Collate=False" }]);
        let status = cmd
            .arg(pdf_path)
            .status()
            .map_err(|e| format!("Failed to invoke `lp`: {e}"))?;
        if status.success() {
            Ok(format!("Sent print job to CUPS (`lp`) for {}", pdf_path.display()))
        } else {
            let _ = Command::new("open").args(["-a", "Preview"]).arg(pdf_path).status();
            Ok(format!("Opened {} in Preview for printing", pdf_path.display()))
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut cmd = Command::new("lp");
        if let Some(ref p) = opts.printer_name {
            if !p.trim().is_empty() {
                cmd.args(["-d", p]);
            }
        }
        cmd.args(["-n", &opts.copies.clamp(1, 99).to_string()]);
        if !opts.all_pages && !opts.page_range.trim().is_empty() {
            cmd.args(["-P", opts.page_range.trim()]);
        }
        if opts.orientation == PageOrientation::Landscape {
            cmd.args(["-o", "landscape"]);
        }
        cmd.args(["-o", &format!("media={}", cups_paper_name(opts.paper_size))]);
        let sides = match opts.duplex {
            DuplexMode::Simplex => "one-sided",
            DuplexMode::LongEdge => "two-sided-long-edge",
            DuplexMode::ShortEdge => "two-sided-short-edge",
        };
        cmd.args(["-o", &format!("sides={sides}")]);
        if !opts.color {
            cmd.args(["-o", "ColorModel=Gray"]);
        }
        cmd.args(["-o", if opts.collate { "Collate=True" } else { "Collate=False" }]);
        if let Ok(s) = cmd.arg(pdf_path).status() {
            if s.success() {
                return Ok(format!("Sent print job to `lp` ({})", pdf_path.display()));
            }
        }
        if let Ok(s) = Command::new("lpr").arg(pdf_path).status() {
            if s.success() {
                return Ok(format!("Sent print job to `lpr` ({})", pdf_path.display()));
            }
        }
        Err("No available print spooler (`lp`/`lpr`) succeeded".to_string())
    }
}
