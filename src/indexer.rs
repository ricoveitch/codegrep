extern crate walkdir;
use regex::Regex;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process;
use walkdir::{DirEntry, WalkDir};

use crate::{
    logger,
    utils::{get_absolute_path, path_exists, OptionIterator},
};

fn is_hidden(entry: &DirEntry) -> bool {
    let file_type = entry.file_type();

    entry
        .file_name()
        .to_str()
        .map(|s| {
            s.contains("node_modules")
                || (file_type.is_file()
                    && (!s.ends_with(".js") || s.starts_with(".") || s.contains("test")))
        })
        .unwrap_or(false)
}

type FilePath = String;

struct Index {
    content: Vec<String>,
    fn_offsets: HashMap<String, usize>,
    fn_imports: HashMap<String, FilePath>,
}

impl Index {
    fn find_local_fn_offset(&self, func_name: &str) -> Option<&usize> {
        self.fn_offsets.get(func_name)
    }
}

pub struct Indexer {
    project_dir: String,
    index: HashMap<FilePath, Index>,
    fre: Regex,
    afre: Regex,
    ifre: Regex,
}

impl Indexer {
    pub fn new(project_dir: &str) -> Indexer {
        Indexer {
            project_dir: project_dir.to_string(),
            index: HashMap::new(),
            fre: Regex::new(r"^\s*function\s+(\w*)\s*\(").unwrap(),
            afre: Regex::new(r"^\s*(const|let|var)\s+(\w*)\s+=\s+\(").unwrap(),
            ifre: Regex::new(
                r##"(const|let|var)\s*\{?([\s\w,]+)\}?\s*=\s*require\(['"]([\w\.\/]+)['"]\)"##,
            )
            .unwrap(),
        }
    }

    pub fn index(&mut self) -> Result<(), String> {
        if !path_exists(&self.project_dir) {
            return Err(format!(
                "no such file or directory exists for {}",
                self.project_dir
            ));
        }

        for file in WalkDir::new(&self.project_dir)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
            .filter_map(|file| file.ok())
            .filter(|file| file.file_type().is_file())
        {
            let file_path = file.path().canonicalize().unwrap().display().to_string();
            if let Err(e) = self.index_file(&file_path) {
                return Err(format!("failed to parse file {}", e));
            }
        }

        if self.index.is_empty() {
            return Err(format!("no files were found in {}", self.project_dir));
        }

        Ok(())
    }

    pub fn iter_fn_content(
        &self,
        file_path: &str,
        func_name: &str,
        object: Option<String>,
    ) -> OptionIterator<impl Iterator<Item = &String>> {
        let absolute_path = get_absolute_path(file_path).unwrap();

        // try local functions
        let index = self.get_index(&absolute_path);
        if object.is_none() {
            if let Some(offset) = index.find_local_fn_offset(func_name) {
                return OptionIterator {
                    iter: Some(index.content.iter().skip(*offset)),
                };
            }
        }

        // try imported functions
        let import = match &object {
            Some(object) => object,
            None => func_name,
        };

        let import_path = match index.fn_imports.get(import) {
            Some(p) => p,
            None => {
                logger::warn(&format!(
                    "Unable to find function reference for {} in {}",
                    func_name, file_path
                ));
                return OptionIterator { iter: None };
            }
        };

        let index = self.get_index(&import_path);
        let offset = index.find_local_fn_offset(func_name).unwrap();

        OptionIterator {
            iter: Some(index.content.iter().skip(*offset)),
        }
    }

    fn store_content(&mut self, file_path: &str) -> Result<(), Box<dyn Error>> {
        let content: Vec<String> = fs::read_to_string(file_path)?
            .lines()
            .map(|s| s.trim().to_string())
            .collect();

        self.index.insert(
            file_path.to_string(),
            Index {
                content,
                fn_offsets: HashMap::new(),
                fn_imports: HashMap::new(),
            },
        );
        Ok(())
    }

    fn find_funcs(&self, file_path: &str) -> Result<Vec<(String, usize)>, String> {
        let content = match self.index.get(&file_path.to_string()) {
            Some(c) => &c.content,
            None => return Err("content not found".to_string()),
        };

        let mut funcs = vec![];
        for (line_idx, line) in content.iter().enumerate() {
            if let Some(cap) = self.fre.captures(&line) {
                funcs.push((cap[1].to_string(), line_idx));
            } else if let Some(cap) = self.afre.captures(&line) {
                funcs.push((cap[2].to_string(), line_idx));
            }
        }
        Ok(funcs)
    }

    fn find_fn_imports(&self, file_path: &str) -> Vec<(String, String)> {
        let content = match self.index.get(&file_path.to_string()) {
            Some(c) => &c.content,
            None => process::exit(1),
        };

        let mut funcs = vec![];
        for cap in self.ifre.captures_iter(&content.join("\n")) {
            let jump = cap[3].to_string();
            let func_names: Vec<&str> = cap[2].split(',').collect();
            for fname in func_names {
                funcs.push((fname.trim().to_string(), jump.to_owned()));
            }
        }

        funcs
    }

    fn index_file(&mut self, file_path: &str) -> Result<(), Box<dyn Error>> {
        self.store_content(file_path)?;

        let funcs = self.find_funcs(file_path)?;
        for (func_name, pos) in funcs {
            self.index.entry(file_path.to_string()).and_modify(|f| {
                f.fn_offsets.insert(func_name, pos);
            });
        }

        let imports = self.find_fn_imports(file_path);
        for (func_name, import_path) in imports {
            self.index.entry(file_path.to_string()).and_modify(|f| {
                let path = Path::new(file_path)
                    .parent()
                    .unwrap()
                    .join(format!("{}.js", import_path))
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string();
                f.fn_imports.insert(func_name, path); // fixme: add path
            });
        }

        Ok(())
    }

    fn get_index(&self, path: &str) -> &Index {
        self.index.get(path).unwrap_or_else(|| {
            logger::err(&format!("Failed to to find {} index record", path));
            process::exit(1);
        })
    }
}
