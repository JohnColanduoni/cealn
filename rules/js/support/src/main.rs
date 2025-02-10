use std::{
    env, fs,
    path::{Path, PathBuf},
};

use ress::{
    tokens::{Keyword, Punct, Token},
    Scanner,
};

fn main() {
    let command = env::args().nth(1).unwrap();
    match &*command {
        "enumerate-imports" => {
            let src_file = PathBuf::from(env::args().nth(2).unwrap());
            enumerate_imports(&src_file)
        }
        _ => panic!("unknown command"),
    }
}

fn enumerate_imports(src_file: &Path) {
    let contents = fs::read_to_string(src_file).unwrap();
    let scanner = Scanner::new(&contents);
    let mut state = State::Nothing;
    for item in scanner {
        let item = item.unwrap();
        match state {
            State::Nothing => match item.token {
                Token::Keyword(Keyword::Import(_)) => {
                    state = State::ExpectingImportedValues { brace_depth: 0 };
                }
                Token::Keyword(Keyword::Export(_)) => {
                    state = State::ExpectingExportedValues { brace_depth: 0 };
                }
                _ => {}
            },
            State::ExpectingImportedValues { brace_depth } => {
                match item.token {
                    Token::Ident(ref ident) if ident.as_ref() == "from" => {
                        state = State::ExepctingImportSource;
                    }
                    Token::Ident(_) => {
                        // imported value
                    }
                    Token::Punct(Punct::OpenBrace) => {
                        state = State::ExpectingImportedValues {
                            brace_depth: brace_depth + 1,
                        };
                    }
                    Token::Punct(Punct::CloseBrace) => {
                        if let Some(brace_depth) = brace_depth.checked_sub(1) {
                            state = State::ExpectingImportedValues { brace_depth };
                        } else {
                            eprintln!("warning: failed to parse import (unexpected close brace)");
                            state = State::Nothing;
                        }
                    }
                    Token::Punct(Punct::Asterisk) => {}
                    Token::Punct(Punct::Comma) => {}
                    Token::Punct(Punct::OpenParen) => {
                        state = State::ExepctingImportSource;
                    }
                    Token::String(source) => {
                        // FIXME: handle escapes!
                        match source {
                            ress::tokens::StringLit::Single(source) => {
                                if source.content.contains('\\') {
                                    todo!()
                                }
                                println!("{}", source.content);
                                state = State::Nothing;
                            }
                            ress::tokens::StringLit::Double(source) => {
                                if source.content.contains('\\') {
                                    todo!()
                                }
                                println!("{}", source.content);
                                state = State::Nothing;
                            }
                        }
                    }
                    Token::Comment(_) => {}
                    token => {
                        eprintln!(
                        "warning: failed to parse import, expecting import value (unexpected token {:?} at {}:{}:{})",
                        token, src_file.display(), item.location.start.line, item.location.start.column,
                    );
                        state = State::Nothing;
                    }
                }
            }
            State::ExpectingExportedValues { brace_depth } => {
                match item.token {
                    Token::Ident(ref ident) if ident.as_ref() == "from" => {
                        state = State::ExepctingImportSource;
                    }
                    Token::Ident(_) if brace_depth > 0 => {
                        // exported/imported value
                    }
                    Token::Keyword(Keyword::Default(_)) if brace_depth > 0 => {
                        // exported/imported value
                    }
                    Token::Punct(Punct::OpenBrace) => {
                        state = State::ExpectingExportedValues {
                            brace_depth: brace_depth + 1,
                        };
                    }
                    Token::Punct(Punct::CloseBrace) => {
                        if let Some(brace_depth) = brace_depth.checked_sub(1) {
                            state = State::ExpectingExportedValues { brace_depth };
                        } else {
                            eprintln!("warning: failed to parse import (unexpected close brace)");
                            state = State::Nothing;
                        }
                    }
                    Token::Punct(Punct::Asterisk) => {}
                    Token::Punct(Punct::Comma) => {}
                    Token::Punct(Punct::OpenParen) => {
                        state = State::ExepctingImportSource;
                    }
                    Token::Comment(_) => {}
                    Token::Keyword(_) => {
                        // Not a re-export
                        state = State::Nothing;
                    }
                    Token::Ident(_) if brace_depth == 0 => {
                        // Not a re-export
                        state = State::Nothing;
                    }
                    token => {
                        eprintln!(
                        "warning: failed to parse import, expecting import value (unexpected token {:?} at {}:{}:{})",
                        token, src_file.display(), item.location.start.line, item.location.start.column,
                    );
                        state = State::Nothing;
                    }
                }
            }
            State::ExepctingImportSource => match item.token {
                Token::String(source) => {
                    // FIXME: handle escapes!
                    match source {
                        ress::tokens::StringLit::Single(source) => {
                            if source.content.contains('\\') {
                                todo!()
                            }
                            println!("{}", source.content);
                            state = State::Nothing;
                        }
                        ress::tokens::StringLit::Double(source) => {
                            if source.content.contains('\\') {
                                todo!()
                            }
                            println!("{}", source.content);
                            state = State::Nothing;
                        }
                    }
                }
                Token::Comment(_) => {}
                _ => {
                    // Likely some sort of import expression, ignore
                    state = State::Nothing;
                }
            },
        }
    }
}

enum State {
    Nothing,
    ExpectingImportedValues { brace_depth: usize },
    ExpectingExportedValues { brace_depth: usize },
    ExepctingImportSource,
}
