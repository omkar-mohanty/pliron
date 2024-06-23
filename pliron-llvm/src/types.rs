//! [Type]s defined in the LLVM dialect.

use combine::{between, optional, parser::char::spaces, token, Parser};
use pliron::{
    common_traits::Verify,
    context::{Context, Ptr},
    identifier::Identifier,
    impl_verify_succ, input_err_noloc,
    irfmt::{
        parsers::{delimited_list_parser, int_parser, location, spaced, type_parser},
        printers::{enclosed, list_with_sep},
    },
    location::Located,
    parsable::{IntoParseResult, Parsable, ParseResult, StateStream},
    printable::{self, ListSeparator, Printable},
    r#type::{Type, TypeObj, TypePtr},
    result::Result,
    verify_err_noloc,
};
use pliron_derive::def_type;
use thiserror::Error;

use std::hash::Hash;

/// Represents a c-like struct type.
/// Limitations and warnings on its usage are similar to that in MLIR.
/// `<https://mlir.llvm.org/docs/Dialects/LLVM/#structure-types>`
///   1. Anonymous (aka unnamed) structs cannot be recursive.
///   2. Named structs are uniqued *only* by name, and may be recursive.
///   3. LLVM calls anonymous structs as literal structs and
///      named structs as identified structs.
///   4. Named structs may be opaque, i.e., no body specificed.
///      Recursive types may be created by first creating an opaque struct
///      and later setting its fields (body).
#[def_type("llvm.struct")]
#[derive(Debug)]
pub struct StructType {
    name: Option<String>,
    fields: Option<Vec<Ptr<TypeObj>>>,
}

impl StructType {
    /// Get or create a named StructType.
    /// If `fields` is `None`, it indicates an opaque struct.
    /// A body can be added to opaque structs by calling this again later.
    /// Returns an error if all of the below conditions are true:
    ///   a. The name is already registered
    ///   b. The body is already set (i.e, the struct is not oqaue)
    ///   c. The fields provided here don't match with the existing body.
    /// Since named structs only rely on the name for uniqueness,
    /// It is not an error to provide `fields` as `None` even when
    /// the named struct already exists and has its body set.
    pub fn get_named(
        ctx: &mut Context,
        name: &str,
        fields: Option<Vec<Ptr<TypeObj>>>,
    ) -> Result<TypePtr<Self>> {
        let self_ptr = Type::register_instance(
            StructType {
                name: Some(name.to_string()),
                // Uniquing happens only on the name, so this doesn't matter.
                fields: None,
            },
            ctx,
        );
        // Verify that we created a new or equivalent existing type.
        let mut self_ref = self_ptr.to_ptr().deref_mut(ctx);
        let self_ref = self_ref.downcast_mut::<StructType>().unwrap();
        assert!(self_ref.name.as_ref().unwrap() == name);
        if let Some(fields) = fields {
            // We've been provided fields to be set.
            if let Some(existing_fields) = &self_ref.fields {
                // Fields were already set before, ensure they're same as the given ones.
                if existing_fields != &fields {
                    input_err_noloc!(StructErr::ExistingMismatch(name.into()))?
                }
            } else {
                // Set the fields now.
                self_ref.fields = Some(fields);
            }
        }
        Ok(self_ptr)
    }

    /// Get or create a new unnamed (anonymous) struct.
    /// These are finalized upon creation, and uniqued based on the fields.
    pub fn get_unnamed(ctx: &mut Context, fields: Vec<Ptr<TypeObj>>) -> TypePtr<Self> {
        Type::register_instance(
            StructType {
                name: None,
                fields: Some(fields),
            },
            ctx,
        )
    }

    /// If a named struct already exists, get a pointer to it.
    pub fn get_existing_named(ctx: &Context, name: &str) -> Option<TypePtr<Self>> {
        Type::get_instance(
            StructType {
                name: Some(name.to_string()),
                // Named structs are uniqued only on the name.
                fields: None,
            },
            ctx,
        )
    }

    /// If an unnamed struct already exists, get a pointer to it.
    pub fn get_existing_unnamed(ctx: &Context, fields: Vec<Ptr<TypeObj>>) -> Option<TypePtr<Self>> {
        Type::get_instance(
            StructType {
                name: None,
                fields: Some(fields),
            },
            ctx,
        )
    }

    /// Does this struct not have its body set?
    pub fn is_opaque(&self) -> bool {
        self.fields.is_none()
    }

    /// Is this a named struct?
    pub fn is_named(&self) -> bool {
        self.name.is_some()
    }

    /// Get type of the idx'th field.
    pub fn field_type(&self, field_idx: usize) -> Ptr<TypeObj> {
        self.fields.as_ref().unwrap()[field_idx]
    }

    /// Get the number of fields this struct has
    pub fn num_fields(&self) -> usize {
        self.fields.as_ref().unwrap().len()
    }
}

#[derive(Debug, Error)]
pub enum StructErr {
    #[error("struct cannot be both opaque and anonymous")]
    OpaqueAndAnonymousErr,
    #[error("struct {0} already exists and is different")]
    ExistingMismatch(String),
}

impl Verify for StructType {
    fn verify(&self, _ctx: &Context) -> Result<()> {
        if self.name.is_none() && self.fields.is_none() {
            verify_err_noloc!(StructErr::OpaqueAndAnonymousErr)?
        }
        Ok(())
    }
}

impl Printable for StructType {
    fn fmt(
        &self,
        ctx: &Context,
        state: &printable::State,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        write!(f, "<")?;

        use std::cell::RefCell;
        // Ugly, but also the simplest way to avoid infinite recursion.
        // MLIR does the same: see LLVMTypeSyntax::printStructType.
        thread_local! {
            // We use a vec instead of a HashMap hoping that this isn't
            // going to be large, in which case vec would be faster.
            static IN_PRINTING: RefCell<Vec<String>>  = const { RefCell::new(vec![]) };
        }
        if let Some(name) = &self.name {
            let in_printing = IN_PRINTING.with(|f| f.borrow().contains(name));
            if in_printing {
                return write!(f, "{}>", name.clone());
            }
            IN_PRINTING.with(|f| f.borrow_mut().push(name.clone()));
            write!(f, "{name}")?;
            if !self.is_opaque() {
                write!(f, " ")?;
            }
        }

        if let Some(fields) = &self.fields {
            enclosed(
                "{ ",
                " }",
                list_with_sep(fields, ListSeparator::CharSpace(',')),
            )
            .fmt(ctx, state, f)?;
        }

        // Done processing this struct. Remove it from the stack.
        if let Some(name) = &self.name {
            debug_assert!(IN_PRINTING.with(|f| f.borrow().last().unwrap() == name));
            IN_PRINTING.with(|f| f.borrow_mut().pop());
        }
        write!(f, ">")
    }
}

impl Hash for StructType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match &self.name {
            Some(name) => name.hash(state),
            None => self
                .fields
                .as_ref()
                .expect("Anonymous struct must have its fields set")
                .iter()
                .for_each(|field_type| {
                    field_type.hash(state);
                }),
        }
    }
}

impl PartialEq for StructType {
    fn eq(&self, other: &Self) -> bool {
        match (&self.name, &other.name) {
            (Some(name), Some(other_name)) => name == other_name,
            (None, None) => self.fields == other.fields,
            _ => false,
        }
    }
}

impl Parsable for StructType {
    type Arg = ();
    type Parsed = TypePtr<Self>;

    fn parse<'a>(
        state_stream: &mut StateStream<'a>,
        _arg: Self::Arg,
    ) -> ParseResult<'a, Self::Parsed>
    where
        Self: Sized,
    {
        let body_parser = || {
            // Parse multiple type annotated fields separated by ',', all of it delimited by braces.
            delimited_list_parser('{', '}', ',', Ptr::<TypeObj>::parser(()))
        };

        let named = spaced((location(), Identifier::parser(())))
            .and(optional(spaced(body_parser())))
            .map(|((loc, name), body_opt)| (loc, Some(name), body_opt));
        let anonymous = spaced((location(), body_parser()))
            .map(|(loc, body)| (loc, None::<Identifier>, Some(body)));

        // A struct type is named or anonymous.
        let mut struct_parser = between(token('<'), token('>'), named.or(anonymous));

        let (loc, name_opt, body_opt) = struct_parser.parse_stream(state_stream).into_result()?.0;
        let ctx = &mut state_stream.state.ctx;
        if let Some(name) = name_opt {
            StructType::get_named(ctx, &name, body_opt)
                .map_err(|mut err| {
                    err.set_loc(loc);
                    err
                })
                .into_parse_result()
        } else {
            Ok(StructType::get_unnamed(
                ctx,
                body_opt.expect("Without a name, a struct type must have a body."),
            ))
            .into_parse_result()
        }
    }
}

impl Eq for StructType {}

/// An opaque pointer, corresponding to LLVM's pointer type.
#[def_type("llvm.ptr")]
#[derive(Hash, PartialEq, Eq, Debug)]
pub struct PointerType;

impl PointerType {
    /// Get or create a new pointer type.
    pub fn get(ctx: &mut Context) -> TypePtr<Self> {
        Type::register_instance(PointerType, ctx)
    }
    /// Get, if it already exists, a pointer type.
    pub fn get_existing(ctx: &Context) -> Option<TypePtr<Self>> {
        Type::get_instance(PointerType, ctx)
    }
}

impl Printable for PointerType {
    fn fmt(
        &self,
        _ctx: &Context,
        _state: &printable::State,
        _f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        Ok(())
    }
}

impl Parsable for PointerType {
    type Arg = ();
    type Parsed = TypePtr<Self>;

    fn parse<'a>(
        state_stream: &mut StateStream<'a>,
        _arg: Self::Arg,
    ) -> ParseResult<'a, Self::Parsed>
    where
        Self: Sized,
    {
        Ok(PointerType::get(state_stream.state.ctx)).into_parse_result()
    }
}

impl_verify_succ!(PointerType);

/// Array type, corresponding to LLVM's array type.
#[def_type("llvm.array")]
#[derive(Hash, PartialEq, Eq, Debug)]
pub struct ArrayType {
    elem: Ptr<TypeObj>,
    size: usize,
}

impl ArrayType {
    /// Get or create a new array type.
    pub fn get(ctx: &mut Context, elem: Ptr<TypeObj>, size: usize) -> TypePtr<Self> {
        Type::register_instance(ArrayType { elem, size }, ctx)
    }
    /// Get, if it already exists, an array type.
    pub fn get_existing(ctx: &Context, elem: Ptr<TypeObj>, size: usize) -> Option<TypePtr<Self>> {
        Type::get_instance(ArrayType { elem, size }, ctx)
    }

    /// Get array element type.
    pub fn elem_type(&self) -> Ptr<TypeObj> {
        self.elem
    }

    /// Get array size.
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Printable for ArrayType {
    fn fmt(
        &self,
        ctx: &Context,
        _state: &printable::State,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        write!(f, "[{} x {}]", self.size, self.elem.disp(ctx))
    }
}

impl Parsable for ArrayType {
    type Arg = ();
    type Parsed = TypePtr<Self>;

    fn parse<'a>(
        state_stream: &mut StateStream<'a>,
        _arg: Self::Arg,
    ) -> ParseResult<'a, Self::Parsed>
    where
        Self: Sized,
    {
        combine::between(
            token('['),
            token(']'),
            spaced((int_parser::<usize>(), spaced(token('x')), type_parser())),
        )
        .parse_stream(state_stream)
        .map(|(size, _, elem)| ArrayType::get(state_stream.state.ctx, elem, size))
        .into()
    }
}
impl_verify_succ!(ArrayType);

#[def_type("llvm.void")]
#[derive(Hash, PartialEq, Eq, Debug)]
pub struct VoidType;

impl VoidType {
    /// Get or create a new void type.
    pub fn get(ctx: &mut Context) -> TypePtr<Self> {
        Type::register_instance(Self {}, ctx)
    }
}

impl Printable for VoidType {
    fn fmt(
        &self,
        _ctx: &Context,
        _state: &printable::State,
        _f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        Ok(())
    }
}

impl Parsable for VoidType {
    type Arg = ();

    type Parsed = TypePtr<VoidType>;

    fn parse<'a>(
        state_stream: &mut StateStream<'a>,
        _arg: Self::Arg,
    ) -> ParseResult<'a, Self::Parsed> {
        Ok(VoidType::get(state_stream.state.ctx)).into_parse_result()
    }
}

impl_verify_succ!(VoidType);

#[def_type("llvm.func")]
#[derive(Hash, PartialEq, Eq, Debug)]
pub struct FuncType {
    res: Ptr<TypeObj>,
    args: Vec<Ptr<TypeObj>>,
}

impl FuncType {
    /// Get or create a new Func type.
    pub fn get(ctx: &mut Context, res: Ptr<TypeObj>, args: Vec<Ptr<TypeObj>>) -> TypePtr<Self> {
        Type::register_instance(FuncType { res, args }, ctx)
    }
}

impl_verify_succ!(FuncType);

impl Printable for FuncType {
    fn fmt(
        &self,
        ctx: &Context,
        _state: &printable::State,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(
            f,
            "<{} ({})>",
            self.res.disp(ctx),
            list_with_sep(&self.args, ListSeparator::CharSpace(',')).disp(ctx)
        )
    }
}

impl Parsable for FuncType {
    type Arg = ();

    type Parsed = TypePtr<FuncType>;

    fn parse<'a>(
        state_stream: &mut StateStream<'a>,
        _arg: Self::Arg,
    ) -> ParseResult<'a, Self::Parsed> {
        let type_list_parser = spaced(delimited_list_parser('(', ')', ',', type_parser()));
        spaced(between(
            token('<'),
            token('>'),
            spaces().with(type_parser()).and(type_list_parser),
        ))
        .parse_stream(state_stream)
        .map(|(res, args)| FuncType::get(state_stream.state.ctx, res, args))
        .into_result()
    }
}

pub fn register(ctx: &mut Context) {
    VoidType::register_type_in_dialect(ctx, VoidType::parser_fn);
    ArrayType::register_type_in_dialect(ctx, ArrayType::parser_fn);
    StructType::register_type_in_dialect(ctx, StructType::parser_fn);
    PointerType::register_type_in_dialect(ctx, PointerType::parser_fn);
    FuncType::register_type_in_dialect(ctx, FuncType::parser_fn);
}

#[cfg(test)]
mod tests {

    use crate as llvm;
    use combine::{eof, token, Parser};
    use expect_test::expect;
    use pliron_derive::def_type;

    use crate::types::{FuncType, StructType, VoidType};
    use pliron::{
        builtin::{
            self,
            types::{IntegerType, Signedness},
        },
        context::{Context, Ptr},
        impl_verify_succ,
        irfmt::parsers::{spaced, type_parser},
        location,
        parsable::{self, state_stream_from_iterator, Parsable, ParseResult, StateStream},
        printable::{self, Printable},
        r#type::{Type, TypeObj, TypePtr},
        result::Result,
    };

    #[test]
    fn test_struct() -> Result<()> {
        let mut ctx = Context::new();
        let int64_ptr = IntegerType::get(&mut ctx, 64, Signedness::Signless).into();

        // Create an opaque struct since we want a recursive type.
        let list_struct: Ptr<TypeObj> = StructType::get_named(&mut ctx, "LinkedList", None)?.into();
        assert!(list_struct
            .deref(&ctx)
            .downcast_ref::<StructType>()
            .unwrap()
            .is_opaque());
        let list_struct_ptr = TypedPointerType::get(&mut ctx, list_struct).into();
        let fields = vec![int64_ptr, list_struct_ptr];
        // Set the struct body now.
        StructType::get_named(&mut ctx, "LinkedList", Some(fields))?;
        assert!(!list_struct
            .deref(&ctx)
            .downcast_ref::<StructType>()
            .unwrap()
            .is_opaque());

        let list_struct_2 = StructType::get_existing_named(&ctx, "LinkedList")
            .unwrap()
            .into();
        assert!(list_struct == list_struct_2);
        assert!(StructType::get_existing_named(&ctx, "LinkedList2").is_none());

        assert_eq!(
            list_struct.disp(&ctx).to_string(),
            "llvm.struct<LinkedList { builtin.int<i64>, llvm.typed_ptr<llvm.struct<LinkedList>> }>"
        );

        let head_fields = vec![int64_ptr, list_struct_ptr];
        let head_struct = StructType::get_unnamed(&mut ctx, head_fields.clone());
        let head_struct2 = StructType::get_existing_unnamed(&ctx, head_fields).unwrap();
        assert!(head_struct == head_struct2);
        assert!(StructType::get_existing_unnamed(&ctx, vec![int64_ptr, list_struct]).is_none());

        Ok(())
    }

    /// A pointer type that knows the type it points to.
    /// This used to be in LLVM earlier, but the latest version
    /// is now type-erased (https://llvm.org/docs/OpaquePointers.html)
    #[def_type("llvm.typed_ptr")]
    #[derive(Hash, PartialEq, Eq, Debug)]
    pub struct TypedPointerType {
        to: Ptr<TypeObj>,
    }

    impl TypedPointerType {
        /// Get or create a new pointer type.
        pub fn get(ctx: &mut Context, to: Ptr<TypeObj>) -> TypePtr<Self> {
            Type::register_instance(TypedPointerType { to }, ctx)
        }
        /// Get, if it already exists, a pointer type.
        pub fn get_existing(ctx: &Context, to: Ptr<TypeObj>) -> Option<TypePtr<Self>> {
            Type::get_instance(TypedPointerType { to }, ctx)
        }

        /// Get the pointee type.
        pub fn get_pointee_type(&self) -> Ptr<TypeObj> {
            self.to
        }
    }

    impl Printable for TypedPointerType {
        fn fmt(
            &self,
            ctx: &Context,
            _state: &printable::State,
            f: &mut core::fmt::Formatter<'_>,
        ) -> core::fmt::Result {
            write!(f, "<{}>", self.to.disp(ctx))
        }
    }

    impl Parsable for TypedPointerType {
        type Arg = ();
        type Parsed = TypePtr<Self>;

        fn parse<'a>(
            state_stream: &mut StateStream<'a>,
            _arg: Self::Arg,
        ) -> ParseResult<'a, Self::Parsed>
        where
            Self: Sized,
        {
            combine::between(token('<'), token('>'), spaced(type_parser()))
                .parse_stream(state_stream)
                .map(|pointee_ty| TypedPointerType::get(state_stream.state.ctx, pointee_ty))
                .into()
        }
    }

    impl_verify_succ!(TypedPointerType);

    #[test]
    fn test_pointer_types() {
        let mut ctx = Context::new();
        let int32_1_ptr = IntegerType::get(&mut ctx, 32, Signedness::Signed);
        let int64_ptr = IntegerType::get(&mut ctx, 64, Signedness::Signed).into();

        let int64pointer_ptr = TypedPointerType { to: int64_ptr };
        let int64pointer_ptr = Type::register_instance(int64pointer_ptr, &mut ctx);
        assert_eq!(
            int64pointer_ptr.disp(&ctx).to_string(),
            "llvm.typed_ptr<builtin.int<si64>>"
        );
        assert!(int64pointer_ptr == TypedPointerType::get(&mut ctx, int64_ptr));

        assert!(
            int64_ptr
                .deref(&ctx)
                .downcast_ref::<IntegerType>()
                .unwrap()
                .get_width()
                == 64
        );

        assert!(IntegerType::get_existing(&ctx, 32, Signedness::Signed).unwrap() == int32_1_ptr);
        assert!(TypedPointerType::get_existing(&ctx, int64_ptr).unwrap() == int64pointer_ptr);
        assert!(int64pointer_ptr.deref(&ctx).get_pointee_type() == int64_ptr);
    }

    #[test]
    fn test_pointer_type_parsing() {
        let mut ctx = Context::new();
        builtin::register(&mut ctx);
        llvm::register(&mut ctx);
        TypedPointerType::register_type_in_dialect(&mut ctx, TypedPointerType::parser_fn);

        let state_stream = state_stream_from_iterator(
            "llvm.typed_ptr <builtin.int <si64>>".chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );

        let res = type_parser().parse(state_stream).unwrap().0;
        assert_eq!(
            &res.disp(&ctx).to_string(),
            "llvm.typed_ptr<builtin.int<si64>>"
        );
    }

    #[test]
    fn test_struct_type_parsing() {
        let mut ctx = Context::new();
        builtin::register(&mut ctx);
        llvm::register(&mut ctx);
        TypedPointerType::register_type_in_dialect(&mut ctx, TypedPointerType::parser_fn);

        let state_stream = state_stream_from_iterator(
            "llvm.struct<LinkedList { builtin.int<i64>, llvm.typed_ptr<llvm.struct<LinkedList>> }>"
                .chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );

        let res = type_parser().parse(state_stream).unwrap().0;
        assert_eq!(
            &res.disp(&ctx).to_string(),
            "llvm.struct<LinkedList { builtin.int<i64>, llvm.typed_ptr<llvm.struct<LinkedList>> }>"
        );

        // Test parsing an opaque struct.
        let test_string = "llvm.struct<ExternStruct>";
        let state_stream = state_stream_from_iterator(
            test_string.chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );
        let res = type_parser().parse(state_stream).unwrap().0;
        assert_eq!(&res.disp(&ctx).to_string(), test_string);
        {
            let res = res.deref(&ctx);
            let res = res.downcast_ref::<StructType>().unwrap();
            assert!(res.is_opaque() && res.is_named());
        }

        // Test parsing an unnamed struct.
        let test_string = "llvm.struct<{ builtin.int<i8> }>";
        let state_stream = state_stream_from_iterator(
            test_string.chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );
        let res = type_parser().parse(state_stream).unwrap().0;
        assert_eq!(&res.disp(&ctx).to_string(), test_string);
        {
            let res = res.deref(&ctx);
            let res = res.downcast_ref::<StructType>().unwrap();
            assert!(!res.is_opaque() && !res.is_named());
        }
    }

    #[test]
    fn test_struct_type_errs() {
        let mut ctx = Context::new();
        builtin::register(&mut ctx);
        llvm::register(&mut ctx);

        let state_stream = state_stream_from_iterator(
            "llvm.struct < My1 { builtin.int<i8> } >".chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );
        let _ = type_parser().parse(state_stream).unwrap().0;

        let state_stream = state_stream_from_iterator(
            "llvm.struct < My1 { builtin.int<i16> } >".chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );

        let res = type_parser().parse(state_stream);
        let err_msg = format!("{}", res.err().unwrap());

        let expected_err_msg = expect![[r#"
            Parse error at line: 1, column: 15
            struct My1 already exists and is different
        "#]];
        expected_err_msg.assert_eq(&err_msg);
    }

    #[test]
    fn test_functype_parsing() {
        let mut ctx = Context::new();
        builtin::register(&mut ctx);
        llvm::register(&mut ctx);

        let si32 = IntegerType::get(&mut ctx, 32, Signedness::Signed);

        let input = "llvm.func<llvm.void (builtin.int<si32>)>";
        let state_stream = state_stream_from_iterator(
            input.chars(),
            parsable::State::new(&mut ctx, location::Source::InMemory),
        );

        let res = type_parser().and(eof()).parse(state_stream).unwrap().0 .0;

        let void_ty = VoidType::get(&mut ctx);
        assert!(res == FuncType::get(&mut ctx, void_ty.to_ptr(), vec![si32.into()]).into());
        assert_eq!(input, &res.disp(&ctx).to_string());
    }
}
