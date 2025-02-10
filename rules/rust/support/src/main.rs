use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use regex::{Regex, RegexSet};

fn main() {
    let command = std::env::args().nth(1).unwrap();
    match &*command {
        "build-script" => build_script(),
        "rlink-parse" => rlink_parse(),
        "run-test" => run_test(),
        _ => panic!("unknown command"),
    }
}

fn build_script() {
    let mut args = std::env::args().skip(2);
    let exe_name = args.next().unwrap();
    let respfile_path = PathBuf::from(args.next().unwrap());
    let envfile_path = PathBuf::from(args.next().unwrap());
    let linker_respfile_path = PathBuf::from(args.next().unwrap());

    fs::create_dir_all(respfile_path.parent().unwrap()).unwrap();
    let mut respfile = BufWriter::new(File::create(&respfile_path).unwrap());
    fs::create_dir_all(envfile_path.parent().unwrap()).unwrap();
    let mut envfile = BufWriter::new(File::create(&envfile_path).unwrap());
    fs::create_dir_all(linker_respfile_path.parent().unwrap()).unwrap();
    let mut linker_respfile = BufWriter::new(File::create(&linker_respfile_path).unwrap());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::create_dir_all(&out_dir).unwrap();

    let mut child = Command::new(&exe_name).stdout(Stdio::piped()).spawn().unwrap();

    let linker_is_windows = env::var("TARGET").unwrap().contains("windows");

    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        let Some(m) = CARGO_LINE_REGEX.matches(&line).iter().next() else {
            continue;
        };
        let m = CARGO_LINE_REGEXES[m].captures(&line).unwrap();
        match m.name("directive").unwrap().as_str() {
            "rerun-if-changed" => {
                // FIXME: handle this
            }
            "rerun-if-env-changed" => {
                // cealn will rerun if any environment variables are changed
            }
            "rustc-cfg" => {
                writeln!(&mut respfile, "--cfg").unwrap();
                writeln!(&mut respfile, "{}", m.name("kv").unwrap().as_str()).unwrap();
            }
            "rustc-env" => {
                writeln!(&mut envfile, "{}", m.name("kv").unwrap().as_str()).unwrap();
            }
            "rustc-link-lib" => {
                writeln!(&mut respfile, "-l").unwrap();
                writeln!(&mut respfile, "{}", m.name("spec").unwrap().as_str()).unwrap();
                match m.name("kind").map(|x| x.as_str()) {
                    // FIXME: specify static/dylib preference
                    Some("static") | None => {
                        if linker_is_windows {
                            writeln!(&mut linker_respfile, "{}.lib", m.name("name").unwrap().as_str()).unwrap()
                        } else {
                            writeln!(&mut linker_respfile, "-l{}", m.name("name").unwrap().as_str()).unwrap()
                        }
                    }
                    Some("dylib") => {
                        if linker_is_windows {
                            writeln!(&mut linker_respfile, "{}.lib", m.name("name").unwrap().as_str()).unwrap()
                        } else {
                            writeln!(&mut linker_respfile, "-l{}", m.name("name").unwrap().as_str()).unwrap()
                        }
                    }
                    kind => panic!("unsupported link kind {:?}", kind),
                }
            }
            "rustc-link-search" => {
                let kind = m.name("kind").map(|x| x.as_str());
                writeln!(&mut respfile, "-L").unwrap();
                writeln!(&mut respfile, "{}", m.name("spec").unwrap().as_str()).unwrap();
                if kind == Some("native") || kind == Some("all") || kind == None {
                    if linker_is_windows {
                        writeln!(&mut linker_respfile, "/LIBPATH:{}", m.name("name").unwrap().as_str()).unwrap();
                    } else {
                        writeln!(&mut linker_respfile, "-L{}", m.name("name").unwrap().as_str()).unwrap();
                    }
                }
            }
            "rustc-link-arg" => {
                writeln!(&mut linker_respfile, "{}", m.name("arg").unwrap().as_str()).unwrap();
            }
            "rustc-link-arg-tests" => {
                // FIXME
            }
            "warning" => {
                eprintln!("{}", m.name("message").unwrap().as_str());
            }
            directive if directive.starts_with("rustc-") => panic!("unknown cargo directive {:?}", directive),
            _directive => {
                // FIXME: implement metadata
            }
        }
    }

    let status = child.wait().unwrap();
    if !status.success() {
        std::process::exit(1);
    }
}

fn rlink_parse() {
    let rlink_file = PathBuf::from(std::env::args_os().nth(2).unwrap());
    let linker_respfile_path = PathBuf::from(std::env::args_os().nth(3).unwrap());

    fs::create_dir_all(linker_respfile_path.parent().unwrap()).unwrap();
    let mut linker_respfile = BufWriter::new(File::create(&linker_respfile_path).unwrap());

    let rlink_raw = fs::read(&rlink_file).unwrap();

    // FIXME: hilariously bad, don't do this
    for m in RLINK_DEP_REGEX.find_iter(&rlink_raw) {
        let lib_path = std::str::from_utf8(m.as_bytes()).unwrap();
        if !lib_path.contains(".rust") {
            continue;
        }
        writeln!(&mut linker_respfile, "{}", lib_path).unwrap();
    }
    linker_respfile.flush().unwrap();
}

fn run_test() {
    let mut args = std::env::args().skip(2);
    let exe_name = args.next().unwrap();

    // FIXME: hack
    // Find dylibs
    let mut search_paths = BTreeSet::new();
    for entry in glob::glob("/src/target/build/**/*.so").unwrap() {
        let entry = entry.unwrap();
        search_paths.insert(entry.parent().unwrap().to_owned());
    }
    let mut ld_library_path = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_else(|| OsString::from(""));
    for path in search_paths {
        if !ld_library_path.is_empty() {
            ld_library_path.push(":");
        }
        ld_library_path.push(&path);
    }
    std::env::set_var("LD_LIBRARY_PATH", &ld_library_path);

    let status = Command::new(&exe_name).status().unwrap();
    if let Some(code) = status.code() {
        std::process::exit(code);
    } else {
        eprintln!("test process crashed: {}", status);
        std::process::exit(1);
    }
}

const CARGO_LINE_REGEX_PATTERNS: &[&str] = &[
    "(?x)^cargo:(?P<directive> rerun-if-changed) = (?P<path> .*)$",
    "(?x)^cargo:(?P<directive> rerun-if-env-changed) = (?P<name> .*)$",
    "(?x)^cargo:(?P<directive> rustc-cfg) = (?P<kv> .+)?$",
    "(?x)^cargo:(?P<directive> rustc-env) = (?P<kv> .+)?$",
    "(?x)^cargo:(?P<directive> rustc-link-lib) = (?P<spec> ((?P<kind> .+) =)? (?P<name> .+) )$",
    "(?x)^cargo:(?P<directive> rustc-link-search) = (?P<spec> ((?P<kind> .+) =)? (?P<name> .+) )$",
    "(?x)^cargo:(?P<directive> rustc-link-arg) = (?P<arg> .+)$",
    "(?x)^cargo:(?P<directive> rustc-link-arg-tests) = (?P<arg> .+)$",
    "(?x)^cargo:(?P<directive> warning) = (?P<message>.*)$",
    "(?x)^cargo:(?P<directive> rustc-[^=]+).*$",
    "(?x)^cargo:(?P<directive> [^=]+).*$",
];

lazy_static::lazy_static! {
    static ref CARGO_LINE_REGEX: RegexSet = RegexSet::new(CARGO_LINE_REGEX_PATTERNS).unwrap();
}

lazy_static::lazy_static! {
    static ref CARGO_LINE_REGEXES: Vec<Regex> = CARGO_LINE_REGEX_PATTERNS.iter().map(|x| Regex::new(x).unwrap()).collect();
}

lazy_static::lazy_static! {
    static ref RLINK_DEP_REGEX: regex::bytes::Regex = regex::bytes::Regex::new(r#"/[ -~]+\.rlib"#).unwrap();
}
