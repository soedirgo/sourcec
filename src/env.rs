use anyhow::{anyhow, Error};
use inkwell::values::PointerValue;
use serde_json::Value;

use std::{collections::HashMap, rc::Rc};

pub struct Env<'ctx> {
    pub names: HashMap<String, u64>,
    pub parent: Option<Rc<Env<'ctx>>>,
    pub ptr: Option<Rc<PointerValue<'ctx>>>,
    counter: u64,
}

impl<'ctx> Env<'ctx> {
    pub fn new(parent: Option<Rc<Env<'ctx>>>) -> Self {
        Env {
            names: HashMap::new(),
            parent,
            ptr: None,
            counter: 0,
        }
    }

    pub fn add_name(&mut self, name: String) {
        self.counter += 1;
        self.names.insert(name, self.counter);
    }

    pub fn lookup(&self, name: &str) -> Result<(usize, u64), Error> {
        if let Some(&offset) = self.names.get(name) {
            return Ok((0, offset));
        }

        let mut jumps = 1;
        let mut frame = self.parent.clone().unwrap();

        loop {
            if let Some(&offset) = frame.names.get(name) {
                break Ok((jumps, offset));
            } else if let Some(parent) = frame.parent.clone() {
                frame = parent;
                jumps += 1;
            } else {
                break Err(anyhow!(format!("Cannot find name {}", name)));
            }
        }
    }

    pub fn add_and_count_decls(&mut self, body: &[Value]) -> Result<u64, Error> {
        let mut count = 0;

        body.iter().for_each(
            |es_node| match es_node.get("type").unwrap().as_str().unwrap() {
                "VariableDeclaration" => {
                    count += 1;
                    let name = es_node
                        .get("declarations")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .get(0)
                        .unwrap()
                        .get("id")
                        .unwrap()
                        .get("name")
                        .unwrap()
                        .as_str()
                        .unwrap();
                    self.add_name(name.into());
                }
                "FunctionDeclaration" => {
                    count += 1;
                    let name = es_node
                        .get("id")
                        .unwrap()
                        .get("name")
                        .unwrap()
                        .as_str()
                        .unwrap();
                    self.add_name(name.into());
                }
                _ => {}
            },
        );

        Ok(count)
    }
}
