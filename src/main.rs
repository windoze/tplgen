use anyhow::Result;
use handlebars::Handlebars;
use log::{debug, info, warn, LevelFilter};
use serde::Deserialize;
use serde_json::{from_reader, to_value, Value};
use walkdir::{WalkDir, DirEntry};
use std::{fs::File, io::BufReader, path::{PathBuf, Path, self}};
use structopt::StructOpt;

/// Log if `Result` is an error
pub trait Logged {
    fn log(self) -> Self;
}

impl<T, E> Logged for Result<T, E>
where
    E: std::fmt::Display,
{
    fn log(self) -> Self {
        if let Err(e) = &self {
            warn!("{}", e)
        }
        self
    }
}

#[derive(Debug, StructOpt, Deserialize)]
#[structopt(name = "tplgen", about = "Template generator")]
#[serde(rename_all = "kebab-case")]
struct Opt {
    /// Verbose output
    #[structopt(short, long)]
    verbose: bool,

    /// Output directory, current directory if not present
    #[structopt(short, long, default_value = ".", parse(from_os_str))]
    output: PathBuf,

    /// Value file in JSON or YAML format, determined by its extension
    #[structopt(short = "i", long = "values", parse(from_os_str))]
    values: Option<PathBuf>,

    /// Do not use environment variables
    #[structopt(short, long)]
    no_env: bool,

    /// Output directory, current directory if not present
    #[structopt(short, long, default_value = ".hbs")]
    extension: String,

    /// Directory or file name of the template files
    input: Vec<PathBuf>,
}

impl Opt {
    fn get_ext(&self) -> String {
        if self.extension.starts_with('.') {
            self.extension.to_owned()
        } else {
            format!(".{}", self.extension)
        }
    }
}

#[derive(Debug)]
struct App {
    data: Value,
    opt: Opt,
    engine: Handlebars<'static>,
}

impl App {
    fn new() -> Self {
        let opt = Opt::from_args();
        Self::init_logger(opt.verbose);
        let data = Self::get_data(&opt);
        let engine = Self::get_engine(&opt);
        Self { data, opt, engine }
    }

    fn init_logger(verbose: bool) {
        let mut b = env_logger::builder();
        if verbose {
            b.filter_level(LevelFilter::Info)
        } else {
            b.filter_level(LevelFilter::Warn)
        }
        // .format_timestamp(None)
        .format_module_path(false)
        // .format_level(false)
        .format_target(false)
        .init();
    }

    fn get_data(opt: &Opt) -> Value {
        let def = serde_json::Value::Object(serde_json::Map::default());
        let obj: Value = match &opt.values {
            Some(path) => {
                if let Ok(file) = File::open(path) {
                    let reader = BufReader::new(file);
                    let ext = path.extension().unwrap_or_default().to_ascii_lowercase();
                    if (ext == "yaml") || (ext == "yml") {
                        let yaml_value: serde_yaml::Result<serde_yaml::Value> =
                            serde_yaml::from_reader(reader).log();
                        if let Ok(v) = yaml_value {
                            to_value(v).log().unwrap_or_default()
                        } else {
                            def
                        }
                    } else {
                        if ext != "json" {
                            // Warning
                        }
                        from_reader(reader).log().unwrap_or(def)
                    }
                } else {
                    warn!("Cannot read value file {}", path.to_string_lossy());
                    def
                }
            }
            None => def,
        };

        if !opt.no_env {
            debug!("Using environment variables");
            let mut mapping = match obj {
                Value::Object(m) => m,
                _ => {
                    warn!("Value file is not a map.");
                    Default::default()
                }
            };
            for (k, v) in std::env::vars() {
                mapping.insert(k, Value::String(v));
            }
            Value::Object(mapping)
        } else {
            debug!("Not using environment variables");
            obj
        }
    }

    fn get_engine(opt: &Opt) -> Handlebars<'static> {
        let ext = opt.get_ext();
        let mut h = Handlebars::new();
        for input in &opt.input {
            debug!("Scanning input {}", input.to_string_lossy());
            Self::register_templates(&mut h, &ext, input).log().ok();
        }
        h
    }

    fn filter_file(entry: &DirEntry, suffix: &str) -> bool {
        let path = entry.path();

        // ignore vim temp files, emacs buffers and files with wrong suffix
        !path.is_file()
            || path
                .file_name()
                .map(|s| {
                    let ds = s.to_string_lossy();
                    ds.starts_with('~') || ds.starts_with('#') || !ds.ends_with(suffix)
                })
                .unwrap_or(true)
    }

    fn register_templates<P>(
        registry: &mut Handlebars<'static>,
        tpl_extension: &str,
        dir_path: P,
    ) -> Result<(), handlebars::TemplateError>
    where
        P: AsRef<Path>,
    {
        if dir_path.as_ref().is_file() {
            let tpl_name = dir_path.as_ref().file_stem().unwrap_or_default().to_string_lossy();
            registry.register_template_file(&tpl_name, &dir_path)?;
            info!("Found template {}", dir_path.as_ref().to_string_lossy());
            return Ok(());
        }

        let dir_path = dir_path.as_ref();

        let prefix_len = if dir_path
            .to_string_lossy()
            .ends_with(|c| c == '\\' || c == '/')
        // `/` will work on windows too so we still need to check
        {
            dir_path.to_string_lossy().len()
        } else {
            dir_path.to_string_lossy().len() + 1
        };

        let walker = WalkDir::new(dir_path).follow_links(true);
        let dir_iter = walker
            .min_depth(1)
            .into_iter()
            .filter(|e| e.is_ok() && !Self::filter_file(e.as_ref().unwrap(), tpl_extension));

        for entry in dir_iter {
            let entry = entry?;

            let tpl_path = entry.path();
            let tpl_file_path = entry.path().to_string_lossy();

            let tpl_name = &tpl_file_path[prefix_len..tpl_file_path.len() - tpl_extension.len()];
            // replace platform path separator with our internal one
            let tpl_canonical_name = tpl_name.replace(path::MAIN_SEPARATOR, "/");
            registry.register_template_file(&tpl_canonical_name, &tpl_path)?;
            info!("Found template {}", tpl_file_path);
        }

        Ok(())
    }

    fn generate(&self) {
        let ext = self.opt.get_ext();
        for name in self.engine.get_templates().keys() {
            let out_path = self.opt.output.join(name);
            info!("{}{} => {}", name, ext, out_path.to_string_lossy());
            if let Some(path) = out_path.parent() {
                std::fs::create_dir_all(path).log().ok();
            };
            if let Ok(f) = File::create(&out_path).log() {
                self.engine.render_to_write(name, &self.data, f).log().ok();
            } else {
                warn!("Failed to write output file {}", out_path.to_string_lossy());
            }
        }
    }
}

fn main() {
    let app = App::new();
    app.generate();
}
