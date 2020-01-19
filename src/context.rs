use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use serde_json::value::{to_value, Map, Value as Json};

use crate::block::{BlockContext, BlockParamHolder};
use crate::error::RenderError;
use crate::grammar::Rule;
use crate::json::path::*;
use crate::json::value::ScopedJson;

pub type Object = HashMap<String, Json>;

/// The context wrap data you render on your templates.
///
#[derive(Debug, Clone)]
pub struct Context {
    data: Json,
}

#[derive(Debug)]
enum ResolvedPath<'a> {
    // FIXME: change to borrowed when possible
    // full path
    AbsolutePath(Vec<String>),
    // relative path and path root
    RelativePath(Vec<String>),
    // relative path against block param value
    BlockParamValue(Vec<String>, &'a Json),
}

fn parse_json_visitor<'a>(
    relative_path: &[PathSeg],
    block_contexts: &'a VecDeque<BlockContext>,
    always_for_absolute_path: bool,
) -> Result<ResolvedPath<'a>, RenderError> {
    let mut path_context_depth: i64 = 0;
    let mut with_block_param = None;
    let mut from_root = false;

    // peek relative_path for block param, @root and  "../../"
    for path_seg in relative_path {
        match path_seg {
            PathSeg::Named(the_path) => {
                if let Some(holder) = get_in_block_params(&block_contexts, the_path) {
                    with_block_param = Some(holder);
                }
                break;
            }
            PathSeg::Ruled(the_rule) => match the_rule {
                Rule::path_root => {
                    from_root = true;
                    break;
                }
                Rule::path_up => path_context_depth += 1,
                _ => break,
            },
        }
    }

    let mut path_stack = Vec::with_capacity(relative_path.len() + 5);
    match with_block_param {
        Some(BlockParamHolder::Value(ref value)) => {
            merge_json_path(&mut path_stack, &relative_path[1..]);
            Ok(ResolvedPath::BlockParamValue(path_stack, value))
        }
        Some(BlockParamHolder::Path(ref paths)) => {
            // TODO: if this path equals base_path, skip this step and make it a ResolvedPath::RelativePath
            path_stack.extend_from_slice(paths);
            merge_json_path(&mut path_stack, &relative_path[1..]);

            Ok(ResolvedPath::AbsolutePath(path_stack))
        }
        None => {
            if path_context_depth > 0 {
                if let Some(ref context_base_path) = block_contexts
                    .get(path_context_depth as usize)
                    .map(|blk| blk.base_path())
                {
                    path_stack.extend_from_slice(context_base_path);
                } else {
                    // TODO: is this correct behaviour?
                    if let Some(ref base_path) = block_contexts.front().map(|blk| blk.base_path()) {
                        path_stack.extend_from_slice(base_path);
                    }
                }
                merge_json_path(&mut path_stack, relative_path);
                Ok(ResolvedPath::AbsolutePath(path_stack))
            } else if from_root {
                merge_json_path(&mut path_stack, relative_path);
                Ok(ResolvedPath::AbsolutePath(path_stack))
            } else if always_for_absolute_path {
                if let Some(ref base_path) = block_contexts.front().map(|blk| blk.base_path()) {
                    path_stack.extend_from_slice(base_path);
                }
                merge_json_path(&mut path_stack, relative_path);
                Ok(ResolvedPath::AbsolutePath(path_stack))
            } else {
                merge_json_path(&mut path_stack, relative_path);
                Ok(ResolvedPath::RelativePath(path_stack))
            }
        }
    }
}

fn get_data<'a>(d: Option<&'a Json>, p: &str) -> Result<Option<&'a Json>, RenderError> {
    let result = match d {
        Some(&Json::Array(ref l)) => p
            .parse::<usize>()
            .map_err(RenderError::with)
            .map(|idx_u| l.get(idx_u))?,
        Some(&Json::Object(ref m)) => m.get(p),
        Some(_) => None,
        None => None,
    };
    Ok(result)
}

fn get_in_block_params<'a>(
    block_contexts: &'a VecDeque<BlockContext>,
    p: &str,
) -> Option<&'a BlockParamHolder> {
    for bc in block_contexts {
        let v = bc.get_block_param(p);
        if v.is_some() {
            return v;
        }
    }

    None
}

pub(crate) fn merge_json(base: &Json, addition: &HashMap<&&str, &Json>) -> Json {
    let mut base_map = match base {
        Json::Object(ref m) => m.clone(),
        _ => Map::new(),
    };

    for (k, v) in addition.iter() {
        base_map.insert((*(*k)).to_string(), (*v).clone());
    }

    Json::Object(base_map)
}

impl Context {
    /// Create a context with null data
    pub fn null() -> Context {
        Context { data: Json::Null }
    }

    /// Create a context with given data
    pub fn wraps<T: Serialize>(e: T) -> Result<Context, RenderError> {
        to_value(e)
            .map_err(RenderError::from)
            .map(|d| Context { data: d })
    }

    /// Navigate the context with base path and relative path
    /// Typically you will set base path to `RenderContext.get_path()`
    /// and set relative path to helper argument or so.
    ///
    /// If you want to navigate from top level, set the base path to `"."`
    pub(crate) fn navigate<'reg, 'rc>(
        &'rc self,
        relative_path: &[PathSeg],
        block_contexts: &VecDeque<BlockContext<'reg, 'rc>>,
    ) -> Result<ScopedJson<'reg, 'rc>, RenderError> {
        // always use absolute at the moment until we get base_value lifetime issue fixed
        let resolved_visitor = parse_json_visitor(&relative_path, block_contexts, true)?;

        match resolved_visitor {
            ResolvedPath::AbsolutePath(paths) => {
                let mut ptr = Some(self.data());
                for p in paths.iter() {
                    ptr = get_data(ptr, p)?;
                }

                Ok(ptr
                    .map(|v| ScopedJson::Context(v, paths))
                    .unwrap_or_else(|| ScopedJson::Missing))
            }
            ResolvedPath::RelativePath(_paths) => {
                // relative path is disabled for now
                unreachable!()
                // let mut ptr = block_contexts.front().and_then(|blk| blk.base_value());
                // for p in paths.iter() {
                //     ptr = get_data(ptr, p)?;
                // }

                // Ok(ptr
                //     .map(|v| ScopedJson::Context(v, paths))
                //     .unwrap_or_else(|| ScopedJson::Missing))
            }
            ResolvedPath::BlockParamValue(paths, value) => {
                let mut ptr = Some(value);
                for p in paths.iter() {
                    ptr = get_data(ptr, p)?;
                }
                Ok(ptr
                    .map(|v| ScopedJson::Derived(v.clone()))
                    .unwrap_or_else(|| ScopedJson::Missing))
            }
        }
    }

    pub fn data(&self) -> &Json {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut Json {
        &mut self.data
    }
}

fn join(segs: &VecDeque<&str>, sep: &str) -> String {
    let mut out = String::new();
    let mut iter = segs.iter();
    if let Some(fst) = iter.next() {
        out.push_str(fst);
        for elt in iter {
            out.push_str(sep);
            out.push_str(elt);
        }
    }
    out
}

#[cfg(test)]
mod test {
    use crate::block::{BlockContext, BlockParams};
    use crate::context::{self, Context};
    use crate::error::RenderError;
    use crate::json::path::PathSeg;
    use crate::json::value::{self, ScopedJson};
    use serde_json::value::Map;
    use std::collections::{HashMap, VecDeque};

    fn navigate_from_root<'reg, 'rc>(
        ctx: &'rc Context,
        path: &str,
    ) -> Result<ScopedJson<'reg, 'rc>, RenderError> {
        let relative_path = PathSeg::parse(path).unwrap();
        ctx.navigate(&relative_path, &VecDeque::new())
    }

    #[derive(Serialize)]
    struct Address {
        city: String,
        country: String,
    }

    #[derive(Serialize)]
    struct Person {
        name: String,
        age: i16,
        addr: Address,
        titles: Vec<String>,
    }

    #[test]
    fn test_render() {
        let v = "hello";
        let ctx = Context::wraps(&v.to_string()).unwrap();
        assert_eq!(
            navigate_from_root(&ctx, "this").unwrap().render(),
            v.to_string()
        );
    }

    #[test]
    fn test_navigation() {
        let addr = Address {
            city: "Beijing".to_string(),
            country: "China".to_string(),
        };

        let person = Person {
            name: "Ning Sun".to_string(),
            age: 27,
            addr,
            titles: vec!["programmer".to_string(), "cartographer".to_string()],
        };

        let ctx = Context::wraps(&person).unwrap();
        assert_eq!(
            navigate_from_root(&ctx, "./addr/country").unwrap().render(),
            "China".to_string()
        );
        assert_eq!(
            navigate_from_root(&ctx, "addr.[country]").unwrap().render(),
            "China".to_string()
        );

        let v = true;
        let ctx2 = Context::wraps(&v).unwrap();
        assert_eq!(
            navigate_from_root(&ctx2, "this").unwrap().render(),
            "true".to_string()
        );

        assert_eq!(
            navigate_from_root(&ctx, "titles.[0]").unwrap().render(),
            "programmer".to_string()
        );

        assert_eq!(
            navigate_from_root(&ctx, "age").unwrap().render(),
            "27".to_string()
        );
    }

    #[test]
    fn test_this() {
        let mut map_with_this = Map::new();
        map_with_this.insert("this".to_string(), value::to_json("hello"));
        map_with_this.insert("age".to_string(), value::to_json(5usize));
        let ctx1 = Context::wraps(&map_with_this).unwrap();

        let mut map_without_this = Map::new();
        map_without_this.insert("age".to_string(), value::to_json(4usize));
        let ctx2 = Context::wraps(&map_without_this).unwrap();

        assert_eq!(
            navigate_from_root(&ctx1, "this").unwrap().render(),
            "[object]".to_owned()
        );
        assert_eq!(
            navigate_from_root(&ctx2, "age").unwrap().render(),
            "4".to_owned()
        );
    }

    #[test]
    fn test_merge_json() {
        let map = json!({ "age": 4 });
        let s = "hello".to_owned();
        let mut hash = HashMap::new();
        let v = value::to_json("h1");
        hash.insert(&"tag", &v);

        let ctx_a1 = Context::wraps(&context::merge_json(&map, &hash)).unwrap();
        assert_eq!(
            navigate_from_root(&ctx_a1, "age").unwrap().render(),
            "4".to_owned()
        );
        assert_eq!(
            navigate_from_root(&ctx_a1, "tag").unwrap().render(),
            "h1".to_owned()
        );

        let ctx_a2 = Context::wraps(&context::merge_json(&value::to_json(s), &hash)).unwrap();
        assert_eq!(
            navigate_from_root(&ctx_a2, "this").unwrap().render(),
            "[object]".to_owned()
        );
        assert_eq!(
            navigate_from_root(&ctx_a2, "tag").unwrap().render(),
            "h1".to_owned()
        );
    }

    #[test]
    fn test_key_name_with_this() {
        let m = btreemap! {
            "this_name".to_string() => "the_value".to_string()
        };
        let ctx = Context::wraps(&m).unwrap();
        assert_eq!(
            navigate_from_root(&ctx, "this_name").unwrap().render(),
            "the_value".to_string()
        );
    }

    use serde::ser::Error as SerdeError;
    use serde::{Serialize, Serializer};

    struct UnserializableType {}

    impl Serialize for UnserializableType {
        fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(SerdeError::custom("test"))
        }
    }

    #[test]
    fn test_serialize_error() {
        let d = UnserializableType {};
        assert!(Context::wraps(&d).is_err());
    }

    #[test]
    fn test_root() {
        let m = json!({
            "a" : {
                "b" : {
                    "c" : {
                        "d" : 1
                    }
                }
            },
            "b": 2
        });
        let ctx = Context::wraps(&m).unwrap();
        let mut block = BlockContext::new();
        *block.base_path_mut() = ["a".to_owned(), "b".to_owned()].to_vec();

        let mut blocks = VecDeque::new();
        blocks.push_front(block);

        assert_eq!(
            ctx.navigate(&PathSeg::parse("@root/b").unwrap(), &blocks)
                .unwrap()
                .render(),
            "2".to_string()
        );
    }

    #[test]
    fn test_block_params() {
        let m = json!([{
            "a": [1, 2]
        }, {
            "b": [2, 3]
        }]);

        let ctx = Context::wraps(&m).unwrap();
        let mut block_params = BlockParams::new();
        block_params
            .add_path("z", ["0".to_owned(), "a".to_owned()].to_vec())
            .unwrap();
        block_params.add_value("t", json!("good")).unwrap();

        let mut block = BlockContext::new();
        block.set_block_params(block_params);

        let mut blocks = VecDeque::new();
        blocks.push_front(block);

        assert_eq!(
            ctx.navigate(&PathSeg::parse("z.[1]").unwrap(), &blocks)
                .unwrap()
                .render(),
            "2".to_string()
        );
        assert_eq!(
            ctx.navigate(&PathSeg::parse("t").unwrap(), &blocks)
                .unwrap()
                .render(),
            "good".to_string()
        );
    }
}
