use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use ansiopt::report::{Report, term};
use ansiopt::{Minifier, Options};

const HELP: &str = "\
ansiopt - minify ANSI escape sequences without changing what the terminal shows

USAGE:
    ansiopt [OPTIONS] [FILE]

Reads FILE (or stdin) and writes the minified stream to stdout.

OPTIONS:
    -o, --output <FILE>   Write to FILE instead of stdout
    -r, --assume-reset    Assume the terminal starts with default attributes
        --visual          Only preserve what is visible: spaces, newlines and
                          erased areas ignore attributes they cannot show
        --rep             Use REP (CSI n b) for runs of a repeated character
    -a, --aggressive      Same as --visual --rep
    -c, --compat          Avoid shorthand old terminals may reject
                          (empty parameters, BEL as string terminator)
        --max-params <N>  Most parameters per written sequence [default: 16]
        --stream          Write output as input arrives (for live pipes)
    -s, --stats           Print size statistics to stderr
        --show            Print the result with escapes made visible
    -h, --help            Print this help
    -V, --version         Print the version

VIEWS (printed instead of the result unless -o is given):
        --diff            What changed between each piece of text
        --explain         What happened to every input sequence, and why
        --hotspot         The text shaded by escape bytes spent on it
        --all             In --diff and --explain, also show changes that only
                          shorten the spelling and sequences kept as they were
        --html <FILE>     Write an HTML report combining all of the above
        --color <WHEN>    Color views: auto, always or never [default: auto]
        --width <N>       Columns for the HTML preview [default: 80]
        --height <N>      Rows for the HTML preview [default: from the input]
";

struct Args {
    options: Options,
    input: Option<String>,
    output: Option<String>,
    stream: bool,
    stats: bool,
    show: bool,
    diff: bool,
    explain: bool,
    hotspot: bool,
    all: bool,
    html: Option<String>,
    color: Option<bool>,
    width: Option<u16>,
    height: Option<u16>,
}

impl Args {
    fn has_view(&self) -> bool {
        self.diff || self.explain || self.hotspot || self.html.is_some()
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        options: Options::default(),
        input: None,
        output: None,
        stream: false,
        stats: false,
        show: false,
        diff: false,
        explain: false,
        hotspot: false,
        all: false,
        html: None,
        color: None,
        width: None,
        height: None,
    };
    let mut iter = std::env::args().skip(1);
    let value = |name: &str, iter: &mut dyn Iterator<Item = String>| {
        iter.next().ok_or(format!("{name} needs a value"))
    };
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("ansiopt {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-o" | "--output" => args.output = Some(value("--output", &mut iter)?),
            "-r" | "--assume-reset" => args.options.assume_reset = true,
            "--visual" => args.options.visual = true,
            "--rep" => args.options.rep = true,
            "-a" | "--aggressive" => {
                args.options.visual = true;
                args.options.rep = true;
            }
            "-c" | "--compat" => args.options.compat = true,
            "--max-params" => {
                let n = value("--max-params", &mut iter)?;
                args.options.max_params = n
                    .parse()
                    .map_err(|_| format!("invalid --max-params: {n}"))?;
            }
            "--stream" => args.stream = true,
            "-s" | "--stats" => args.stats = true,
            "--show" => args.show = true,
            "--diff" => args.diff = true,
            "--explain" => args.explain = true,
            "--hotspot" => args.hotspot = true,
            "--all" => args.all = true,
            "--html" => args.html = Some(value("--html", &mut iter)?),
            "--color" => {
                args.color = match value("--color", &mut iter)?.as_str() {
                    "auto" => None,
                    "always" => Some(true),
                    "never" => Some(false),
                    other => return Err(format!("invalid --color: {other}")),
                };
            }
            "--width" | "--height" => {
                let n = value(&arg, &mut iter)?;
                let n: u16 = n
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or(format!("invalid {arg}: {n}"))?;
                if arg == "--width" {
                    args.width = Some(n);
                } else {
                    args.height = Some(n);
                }
            }
            "-" if args.input.is_none() => args.input = Some(arg),
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ if args.input.is_none() => args.input = Some(arg),
            _ => return Err(format!("unexpected argument: {arg}")),
        }
    }
    if args.stream && args.has_view() {
        return Err("--stream can't be combined with views".into());
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ansiopt: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return Ok(()),
        Err(msg) => {
            eprintln!("ansiopt: {msg}\n\n{HELP}");
            std::process::exit(2);
        }
    };

    let mut input: Box<dyn Read> = match args.input.as_deref() {
        None | Some("-") => Box::new(io::stdin().lock()),
        Some(path) => Box::new(std::fs::File::open(path)?),
    };

    let (read, written) = if args.stream {
        let mut output = open_output(&args)?;
        let counts = stream(&mut input, &mut output, &args)?;
        output.flush()?;
        counts
    } else {
        let mut data = Vec::new();
        input.read_to_end(&mut data)?;
        let result = ansiopt::minify_detailed(&data, &args.options, args.has_view());
        let written = result.output.len();
        if !args.has_view() || args.output.is_some() {
            let mut output = open_output(&args)?;
            emit(&mut output, &result.output, args.show)?;
            output.flush()?;
        }
        if args.has_view() {
            views(&args, &data, result)?;
        }
        (data.len(), written)
    };

    if args.stats {
        let saved = read.saturating_sub(written);
        let pct = if read == 0 {
            0.0
        } else {
            saved as f64 * 100.0 / read as f64
        };
        eprintln!("ansiopt: {read} -> {written} bytes ({saved} saved, {pct:.1}%)");
    }
    Ok(())
}

fn open_output(args: &Args) -> io::Result<Box<dyn Write>> {
    Ok(match &args.output {
        Some(path) => Box::new(io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(io::stdout().lock()),
    })
}

fn views(args: &Args, data: &[u8], result: ansiopt::Minified) -> io::Result<()> {
    let report = Report::new(data, &result.output, result.fell_back, result.trace)
        .map_err(|e| io::Error::other(format!("can't line up input and output: {e}")))?;
    let color = args.color.unwrap_or_else(|| {
        io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
    });
    let paint = term::Paint { color };
    let mut stdout = io::stdout().lock();
    let mut first = true;
    let mut section = |text: String| -> io::Result<()> {
        if !first {
            stdout.write_all(b"\n")?;
        }
        first = false;
        stdout.write_all(text.as_bytes())
    };
    if args.diff {
        section(term::diff(&report, paint, args.all))?;
    }
    if args.explain {
        section(term::explain(&report, paint, args.all))?;
    }
    if args.hotspot {
        section(term::hotspot(&report, paint))?;
    }
    if let Some(path) = &args.html {
        let settings = ansiopt::report::html::Settings {
            title: args
                .input
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .map_or("stdin".into(), |n| n.to_string_lossy().into_owned()),
            options: args.options,
            width: args.width,
            height: args.height,
        };
        std::fs::write(path, ansiopt::report::html::html(&report, &settings))?;
        eprintln!("ansiopt: wrote {path}");
    }
    Ok(())
}

fn stream(input: &mut dyn Read, output: &mut dyn Write, args: &Args) -> io::Result<(usize, usize)> {
    let mut minifier = Minifier::new(args.options);
    let mut buf = vec![0; 64 * 1024];
    let (mut read, mut written) = (0, 0);
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        read += n;
        // The producer may pause after any read, so bring the terminal fully
        // up to date each time.
        let mut out = minifier.feed(&buf[..n]);
        out.extend(minifier.flush());
        written += out.len();
        emit(output, &out, args.show)?;
        output.flush()?;
    }
    let out = minifier.finish();
    written += out.len();
    emit(output, &out, args.show)?;
    Ok((read, written))
}

fn emit(output: &mut dyn Write, bytes: &[u8], show: bool) -> io::Result<()> {
    if !show {
        return output.write_all(bytes);
    }
    let lines: Vec<String> = bytes
        .split(|&b| b == b'\n')
        .map(ansiopt::describe::visible)
        .collect();
    output.write_all(lines.join("\\n\n").as_bytes())
}
