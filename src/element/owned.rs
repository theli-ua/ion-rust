// Copyright Amazon.com, Inc. or its affiliates.

use crate::ion_eq::IonEq;
use crate::text::text_formatter::IonValueFormatter;
use crate::types::decimal::Decimal;
use crate::types::integer::Int;
use crate::types::timestamp::Timestamp;
use crate::{IonResult, IonType, ReaderBuilder, Symbol};
use num_bigint::BigInt;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::iter::FromIterator;

use crate::element::builders::{ListBuilder, SExpBuilder, StructBuilder};
use crate::element::iterators::{
    ElementsIterator, FieldIterator, FieldValuesIterator, IndexVec, SymbolsIterator,
};
use crate::element::reader::ElementReader;
use crate::symbol_ref::AsSymbolRef;

impl Element {
    pub fn null(null_type: IonType) -> Element {
        null_type.into()
    }

    pub fn boolean(value: bool) -> Element {
        value.into()
    }

    pub fn string<I: Into<String>>(str: I) -> Element {
        let text: String = str.into();
        text.into()
    }

    pub fn symbol<I: Into<Symbol>>(symbol: I) -> Element {
        let symbol: Symbol = symbol.into();
        symbol.into()
    }

    pub fn integer<I: Into<Int>>(integer: I) -> Element {
        let integer: Int = integer.into();
        integer.into()
    }

    pub fn decimal(decimal: Decimal) -> Element {
        decimal.into()
    }

    pub fn timestamp(timestamp: Timestamp) -> Element {
        timestamp.into()
    }

    pub fn float(float: f64) -> Element {
        float.into()
    }

    pub fn clob<A: AsRef<[u8]>>(bytes: A) -> Element {
        let bytes: &[u8] = bytes.as_ref();
        Value::Clob(bytes.into()).into()
    }

    pub fn blob<A: AsRef<[u8]>>(bytes: A) -> Element {
        let bytes: &[u8] = bytes.as_ref();
        Value::Blob(bytes.into()).into()
    }

    pub fn list_builder() -> ListBuilder {
        ListBuilder::new()
    }

    pub fn sexp_builder() -> SExpBuilder {
        SExpBuilder::new()
    }

    pub fn struct_builder() -> StructBuilder {
        Struct::builder()
    }
}

/// Behavior that is common to both [SExp] and [Struct].
pub trait IonSequence {
    fn elements(&self) -> ElementsIterator<'_>;
    fn get(&self, index: usize) -> Option<&Element>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An in-memory representation of an Ion list
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    children: Vec<Element>,
}

impl List {
    pub(crate) fn new(children: Vec<Element>) -> Self {
        Self { children }
    }

    pub fn builder() -> ListBuilder {
        ListBuilder::new()
    }

    pub fn clone_builder(&self) -> ListBuilder {
        ListBuilder::with_initial_elements(&self.children)
    }
}

impl Display for List {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut ivf = IonValueFormatter { output: f };
        ivf.format_list(self).map_err(|_| std::fmt::Error)?;
        Ok(())
    }
}

impl IonSequence for List {
    fn elements(&self) -> ElementsIterator<'_> {
        ElementsIterator::new(&self.children)
    }

    fn get(&self, index: usize) -> Option<&Element> {
        self.children.get(index)
    }

    fn len(&self) -> usize {
        self.children.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<S: IonSequence> IonEq for S {
    fn ion_eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        for (item1, item2) in self.elements().zip(other.elements()) {
            if !item1.ion_eq(item2) {
                return false;
            }
        }
        true
    }
}

/// An in-memory representation of an Ion s-expression
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SExp {
    children: Vec<Element>,
}

impl SExp {
    pub(crate) fn new(children: Vec<Element>) -> Self {
        Self { children }
    }

    pub fn builder() -> SExpBuilder {
        SExpBuilder::new()
    }

    pub fn clone_builder(&self) -> SExpBuilder {
        SExpBuilder::with_initial_elements(&self.children)
    }
}

impl Display for SExp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut ivf = IonValueFormatter { output: f };
        ivf.format_sexp(self).map_err(|_| std::fmt::Error)?;
        Ok(())
    }
}

impl IonSequence for SExp {
    fn elements(&self) -> ElementsIterator<'_> {
        ElementsIterator::new(&self.children)
    }

    fn get(&self, index: usize) -> Option<&Element> {
        self.children.get(index)
    }

    fn len(&self) -> usize {
        self.children.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// This collection is broken out into its own type to allow instances of it to be shared with Arc/Rc.
#[derive(Debug, Clone)]
struct Fields {
    // Key/value pairs in the order they were inserted
    by_index: Vec<(Symbol, Element)>,
    // Maps symbols to a list of indexes where values may be found in `by_index` above
    by_name: HashMap<Symbol, IndexVec>,
}

impl Fields {
    /// Gets all of the indexes that contain a value associated with the given field name.
    fn get_indexes<A: AsSymbolRef>(&self, field_name: A) -> Option<&IndexVec> {
        field_name
            .as_symbol_ref()
            .text()
            .map(|text| {
                // If the symbol has defined text, look it up by &str
                self.by_name.get(text)
            })
            .unwrap_or_else(|| {
                // Otherwise, construct a (cheap, stack-allocated) Symbol with unknown text...
                let symbol = Symbol::unknown_text();
                // ...and use the unknown text symbol to look up matching field values
                self.by_name.get(&symbol)
            })
    }

    /// Iterates over the values found at the specified indexes.
    fn get_values_at_indexes<'a>(&'a self, indexes: &'a IndexVec) -> FieldValuesIterator<'a> {
        FieldValuesIterator {
            current: 0,
            indexes: Some(indexes),
            by_index: &self.by_index,
        }
    }

    /// Gets the last value in the Struct that is associated with the specified field name.
    ///
    /// Note that the Ion data model views a struct as a bag of (name, value) pairs and does not
    /// have a notion of field ordering. In most use cases, field names are distinct and the last
    /// appearance of a field in the struct's serialized form will have been the _only_ appearance.
    /// If a field name appears more than once, this method makes the arbitrary decision to return
    /// the value associated with the last appearance. If your application uses structs that repeat
    /// field names, you are encouraged to use [get_all] instead.
    fn get_last<A: AsSymbolRef>(&self, field_name: A) -> Option<&Element> {
        self.get_indexes(field_name)
            .and_then(|indexes| indexes.last())
            .and_then(|index| self.by_index.get(*index))
            .map(|(_name, value)| value)
    }

    /// Iterates over all of the values associated with the given field name.
    fn get_all<A: AsSymbolRef>(&self, field_name: A) -> FieldValuesIterator {
        let indexes = self.get_indexes(field_name);
        FieldValuesIterator {
            current: 0,
            indexes,
            by_index: &self.by_index,
        }
    }

    /// Iterates over all of the (field name, field value) pairs in the struct.
    fn iter(&self) -> impl Iterator<Item = &(Symbol, Element)> {
        self.by_index.iter()
    }
}

/// An in-memory representation of an Ion Struct
#[derive(Debug, Clone)]
pub struct Struct {
    fields: Fields,
}

impl Display for Struct {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut ivf = IonValueFormatter { output: f };
        ivf.format_struct(self).map_err(|_| std::fmt::Error)?;
        Ok(())
    }
}

impl Struct {
    pub fn builder() -> StructBuilder {
        StructBuilder::new()
    }

    pub fn clone_builder(&self) -> StructBuilder {
        StructBuilder::with_initial_fields(&self.fields.by_index)
    }

    /// Returns an iterator over the field name/value pairs in this Struct.
    pub fn fields(&self) -> impl Iterator<Item = (&Symbol, &Element)> {
        self.fields
            .iter()
            // Here we convert from &(name, value) to (&name, &value).
            // The former makes a stronger assertion about how the data is being stored. We don't
            // want that to be a mandatory part of the public API.
            .map(|(name, element)| (name, element))
    }

    fn fields_eq(&self, other: &Self) -> bool {
        // For each field name in `self`, get the list of indexes that contain a value with that name.
        for (field_name, field_value_indexes) in &self.fields.by_name {
            let other_value_indexes = match other.fields.get_indexes(field_name) {
                Some(indexes) => indexes,
                // The other struct doesn't have a field with this name so they're not equal.
                None => return false,
            };

            if field_value_indexes.len() != other_value_indexes.len() {
                // The other struct has fields with the same name, but a different number of them.
                return false;
            }

            for field_value in self.fields.get_values_at_indexes(field_value_indexes) {
                if other
                    .fields
                    .get_values_at_indexes(other_value_indexes)
                    .all(|other_value| !field_value.ion_eq(other_value))
                {
                    // Couldn't find an equivalent field in the other struct
                    return false;
                }
            }
        }

        // If all of the above conditions hold, the two structs are equal.
        true
    }

    /// Returns the number of fields in this Struct.
    pub fn len(&self) -> usize {
        self.fields.by_index.len()
    }

    /// Returns `true` if this struct has zero fields.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> FromIterator<(K, V)> for Struct
where
    K: Into<Symbol>,
    V: Into<Element>,
{
    /// Returns an owned struct from the given iterator of field names/values.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut by_index: Vec<(Symbol, Element)> = Vec::new();
        let mut by_name: HashMap<Symbol, IndexVec> = HashMap::new();
        for (field_name, field_value) in iter {
            let field_name = field_name.into();
            let field_value = field_value.into();

            by_name
                .entry(field_name.clone())
                .or_insert_with(IndexVec::new)
                .push(by_index.len());
            by_index.push((field_name, field_value));
        }

        let fields = Fields { by_index, by_name };
        Self { fields }
    }
}

impl Struct {
    pub fn iter(&self) -> FieldIterator<'_> {
        FieldIterator::new(&self.fields.by_index)
    }

    pub fn get<A: AsSymbolRef>(&self, field_name: A) -> Option<&Element> {
        self.fields.get_last(field_name)
    }

    pub fn get_all<A: AsSymbolRef>(&self, field_name: A) -> FieldValuesIterator<'_> {
        self.fields.get_all(field_name)
    }
}

impl PartialEq for Struct {
    fn eq(&self, other: &Self) -> bool {
        // check if both fields have same length
        self.len() == other.len()
            // we need to test equality in both directions for both fields
            // A good example for this is annotated vs not annotated values in struct
            //  { a:4, a:4 } vs. { a:4, a:a::4 } // returns true
            //  { a:4, a:a::4 } vs. { a:4, a:4 } // returns false
            && self.fields_eq(other) && other.fields_eq(self)
    }
}

impl Eq for Struct {}

impl IonEq for Value {
    fn ion_eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Float(f1), Float(f2)) => return f1.ion_eq(f2),
            (Decimal(d1), Decimal(d2)) => return d1.ion_eq(d2),
            (Timestamp(t1), Timestamp(t2)) => return t1.ion_eq(t2),
            (List(l1), List(l2)) => return l1.ion_eq(l2),
            (SExp(s1), SExp(s2)) => return s1.ion_eq(s2),
            _ => {}
        };
        // For any other case, fall back to vanilla equality
        self == other
    }
}

impl IonEq for Element {
    fn ion_eq(&self, other: &Self) -> bool {
        self.annotations == other.annotations && self.value.ion_eq(&other.value)
    }
}

impl IonEq for Vec<Element> {
    fn ion_eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        for (v1, v2) in self.iter().zip(other.iter()) {
            if !v1.ion_eq(v2) {
                return false;
            }
        }
        true
    }
}

/// Variants for all _values_ within an [`Element`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null(IonType),
    Int(Int),
    Float(f64),
    Decimal(Decimal),
    Timestamp(Timestamp),
    String(String),
    Symbol(Symbol),
    Bool(bool),
    Blob(Vec<u8>),
    Clob(Vec<u8>),
    SExp(SExp),
    List(List),
    Struct(Struct),
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut ivf = IonValueFormatter { output: f };
        match &self {
            Value::Null(ion_type) => ivf.format_null(*ion_type),
            Value::Bool(bool) => ivf.format_bool(*bool),
            Value::Int(integer) => ivf.format_integer(integer),
            Value::Float(float) => ivf.format_float(*float),
            Value::Decimal(decimal) => ivf.format_decimal(decimal),
            Value::Timestamp(timestamp) => ivf.format_timestamp(timestamp),
            Value::Symbol(symbol) => ivf.format_symbol(symbol),
            Value::String(string) => ivf.format_string(string),
            Value::Clob(clob) => ivf.format_clob(clob),
            Value::Blob(blob) => ivf.format_blob(blob),
            Value::Struct(struct_) => ivf.format_struct(struct_),
            Value::SExp(sexp) => ivf.format_sexp(sexp),
            Value::List(list) => ivf.format_list(list),
        }
        .map_err(|_| std::fmt::Error)?;

        Ok(())
    }
}

/// An `(annotations, value)` pair representing an Ion value.
#[derive(Debug, Clone)]
pub struct Element {
    annotations: Vec<Symbol>,
    value: Value,
}

impl Element {
    pub fn new(annotations: Vec<Symbol>, value: Value) -> Self {
        Self { annotations, value }
    }

    /// Returns a reference to this [Element]'s [Value].
    ///
    /// ```
    /// use ion_rs::element::owned::{Element, Value};
    /// let element: Element = true.into();
    /// if let Value::Bool(b) = element.value() {
    ///     println!("It was a boolean: {b}");
    /// } else {
    ///     println!("It was something else.");
    /// }
    /// ```
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl Display for Element {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let mut ivf = IonValueFormatter { output: f };

        // display for annotations of this element
        ivf.format_annotations(&self.annotations)
            .map_err(|_| std::fmt::Error)?;

        self.value.fmt(f)
    }
}

impl PartialEq for Element {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.annotations == other.annotations
    }
}

impl Eq for Element {}

// This implementation allows APIs that require an Into<Element> to accept references to an existing
// Element.
impl<'a> From<&'a Element> for Element {
    fn from(element: &'a Element) -> Self {
        element.clone()
    }
}

// Anything that can be turned into a `Value` can then be turned into an `Element`
// by associating it with an empty annotations sequence.
impl<T> From<T> for Element
where
    T: Into<Value>,
{
    fn from(value: T) -> Self {
        Element::new(Vec::new(), value.into())
    }
}

impl From<IonType> for Value {
    fn from(ion_type: IonType) -> Self {
        Value::Null(ion_type)
    }
}

impl From<i64> for Value {
    fn from(i64_val: i64) -> Self {
        Value::Int(Int::I64(i64_val))
    }
}

impl From<BigInt> for Value {
    fn from(big_int_val: BigInt) -> Self {
        Value::Int(Int::BigInt(big_int_val))
    }
}

impl From<Int> for Value {
    fn from(integer_val: Int) -> Self {
        Value::Int(integer_val)
    }
}

impl From<f64> for Value {
    fn from(f64_val: f64) -> Self {
        Value::Float(f64_val)
    }
}

impl From<Decimal> for Value {
    fn from(decimal_val: Decimal) -> Self {
        Value::Decimal(decimal_val)
    }
}

impl From<Timestamp> for Value {
    fn from(timestamp_val: Timestamp) -> Self {
        Value::Timestamp(timestamp_val)
    }
}

impl From<bool> for Value {
    fn from(bool_val: bool) -> Self {
        Value::Bool(bool_val)
    }
}

impl From<&str> for Value {
    fn from(string_val: &str) -> Self {
        Value::String(string_val.to_owned())
    }
}

impl From<String> for Value {
    fn from(string_val: String) -> Self {
        Value::String(string_val)
    }
}

impl From<Symbol> for Value {
    fn from(sym_val: Symbol) -> Self {
        Value::Symbol(sym_val)
    }
}

impl From<&[u8]> for Value {
    fn from(value: &[u8]) -> Self {
        Value::Blob(value.into())
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Value::Blob(value)
    }
}

impl From<List> for Value {
    fn from(list: List) -> Self {
        Value::List(list)
    }
}

impl From<SExp> for Value {
    fn from(s_expr: SExp) -> Self {
        Value::SExp(s_expr)
    }
}

impl From<Struct> for Value {
    fn from(struct_val: Struct) -> Self {
        Value::Struct(struct_val)
    }
}

/// Allows types that can be converted into an Ion [Value] to also specify annotations, producing
/// an [Element].
///
/// ```
/// use ion_rs::ion_list;
/// use ion_rs::element::owned::{Element, IntoAnnotatedElement, Value};
///
/// // Explicit conversion of a Rust bool (`true`) into a `Value`...
/// let boolean_value: Value = true.into();
/// // and then into an `Element`...
/// let mut boolean_element: Element = boolean_value.into();
/// // and then adding annotations to the `Element`.
/// boolean_element = boolean_element.with_annotations(["foo", "bar"]);
///
/// // Much more concise equivalent leveraging the `IntoAnnotatedElement` trait.
/// let boolean_element = true.with_annotations(["foo", "bar"]);
/// ```
pub trait IntoAnnotatedElement: Into<Value> {
    /// Converts the value into an [Element] with the specified annotations.
    fn with_annotations<S: Into<Symbol>, I: IntoIterator<Item = S>>(
        self,
        annotations: I,
    ) -> Element {
        let annotations = annotations.into_iter().map(|i| i.into()).collect();
        Element::new(annotations, self.into())
    }
}

impl<V> IntoAnnotatedElement for V where V: Into<Value> {}

impl Element {
    pub fn ion_type(&self) -> IonType {
        use Value::*;

        match &self.value {
            Null(t) => *t,
            Int(_) => IonType::Int,
            Float(_) => IonType::Float,
            Decimal(_) => IonType::Decimal,
            Timestamp(_) => IonType::Timestamp,
            String(_) => IonType::String,
            Symbol(_) => IonType::Symbol,
            Bool(_) => IonType::Bool,
            Blob(_) => IonType::Blob,
            Clob(_) => IonType::Clob,
            SExp(_) => IonType::SExp,
            List(_) => IonType::List,
            Struct(_) => IonType::Struct,
        }
    }

    pub fn annotations(&self) -> SymbolsIterator<'_> {
        SymbolsIterator::new(&self.annotations)
    }

    pub fn with_annotations<S: Into<Symbol>, I: IntoIterator<Item = S>>(
        self,
        annotations: I,
    ) -> Self {
        let annotations: Vec<Symbol> = annotations.into_iter().map(|i| i.into()).collect();
        Element::new(annotations, self.value)
    }

    pub fn has_annotation(&self, annotation: &str) -> bool {
        self.annotations
            .iter()
            .any(|a| a.text() == Some(annotation))
    }

    pub fn is_null(&self) -> bool {
        matches!(&self.value, Value::Null(_))
    }

    pub fn as_int(&self) -> Option<&Int> {
        match &self.value {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match &self.value {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_decimal(&self) -> Option<&Decimal> {
        match &self.value {
            Value::Decimal(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_timestamp(&self) -> Option<&Timestamp> {
        match &self.value {
            Value::Timestamp(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.value {
            Value::String(text) => Some(text),
            Value::Symbol(sym) => sym.text(),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match &self.value {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_symbol(&self) -> Option<&Symbol> {
        match &self.value {
            Value::Symbol(sym) => Some(sym),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match &self.value {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_lob(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Blob(bytes) | Value::Clob(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub fn as_blob(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Blob(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub fn as_clob(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Clob(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub fn as_sequence(&self) -> Option<&dyn IonSequence> {
        match &self.value {
            Value::SExp(sexp) => Some(sexp),
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    pub fn as_sexp(&self) -> Option<&SExp> {
        match &self.value {
            Value::SExp(sexp) => Some(sexp),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&List> {
        match &self.value {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    pub fn as_struct(&self) -> Option<&Struct> {
        match &self.value {
            Value::Struct(structure) => Some(structure),
            _ => None,
        }
    }

    /// Reads a single Ion [`Element`] from the provided data source.
    ///
    /// If the data source is empty, returns `Ok(None)`.
    /// If the data source has at least one value, returns `Ok(Some(Element))`.
    /// If the data source has invalid data, returns `Err`.
    pub fn read_first<A: AsRef<[u8]>>(data: A) -> IonResult<Option<Element>> {
        let bytes: &[u8] = data.as_ref();
        // Create an iterator over the Elements in the data
        let mut reader = ReaderBuilder::default().build(bytes)?;
        reader.read_next_element()
    }

    /// Reads a single Ion [`Element`] from the provided data source. If the input has invalid
    /// data or does not contain at exactly one Ion value, returns `Err(IonError)`.
    pub fn read_one<A: AsRef<[u8]>>(data: A) -> IonResult<Element> {
        let bytes: &[u8] = data.as_ref();
        // Create an iterator over the Elements in the data
        let mut reader = ReaderBuilder::default().build(bytes)?;
        reader.read_one_element()
    }

    /// Reads all available [`Element`]s from the provided data source.
    ///
    /// If the input has valid data, returns `Ok(Vec<Element>)`.
    /// If the input has invalid data, returns `Err(IonError)`.
    pub fn read_all<A: AsRef<[u8]>>(data: A) -> IonResult<Vec<Element>> {
        let bytes: &[u8] = data.as_ref();
        ReaderBuilder::default().build(bytes)?.elements().collect()
    }
}

#[cfg(test)]
mod value_tests {
    use super::*;
    use crate::{ion_list, ion_sexp, ion_struct};
    use rstest::*;

    #[rstest]
    #[case::strings(
        Element::from("hello"), // An explicitly constructed String Element
        "hello"                 // A Rust &str, which implements Into<Element>
    )]
    #[case::symbols(
        Element::from(Symbol::owned("hello")), // An explicitly constructed Symbol Element
        Symbol::owned("hello")                 // A Symbol, which implements Into<Element>
    )]
    #[case::struct_(
        ion_struct!{"greetings": "hello"},
        Element::read_one(r#"{greetings: "hello"}"#).unwrap()
    )]
    #[case::strings(
        Element::from("hello"), // An explicitly constructed String Element
        "hello"                 // A Rust &str, which implements Into<Element>
    )]
    #[case::symbols(
        Element::from(Symbol::owned("hello")), // An explicitly constructed Symbol Element
        Symbol::owned("hello")                 // A Symbol, which implements Into<Element>
    )]
    #[case::struct_(
        ion_struct!{"greetings": "hello"},
        Element::read_one(r#"{greetings: "hello"}"#).unwrap()
    )]
    fn owned_element_accessors<E1, E2>(#[case] e1: E1, #[case] e2: E2)
    where
        E1: Into<Element>,
        E2: Into<Element>,
    {
        // assert that both element construction methods create the same element
        assert_eq!(e1.into(), e2.into());
    }

    #[rstest]
    #[case::struct_(ion_struct!{"greetings": "hello", "name": "Ion"}, 2)]
    #[case::list(ion_list!["greetings", 5, true], 3)]
    #[case::sexp(ion_sexp!(5 true), 2)]
    fn owned_container_len_test<I: Into<Element>>(#[case] container: I, #[case] length: usize) {
        let container = container.into();
        match container.ion_type() {
            IonType::List | IonType::SExp => {
                // check length for given sequence value
                assert_eq!(container.as_sequence().unwrap().len(), length);
            }
            IonType::Struct => {
                // check length for given struct value
                assert_eq!(container.as_struct().unwrap().len(), length);
            }
            _ => {
                unreachable!("This test is only for container type elements")
            }
        }
    }

    #[rstest]
    #[case::struct_(ion_struct!{"greetings": "hello", "name": "Ion"}, false)]
    #[case::list(ion_list!["greetings", 5, true], false)]
    #[case::list_empty(ion_list![], true)]
    #[case::sexp(ion_sexp!(5 true), false)]
    #[case::sexp_empty(ion_sexp!(), true)]
    fn owned_container_is_empty_test<I: Into<Element>>(
        #[case] container: I,
        #[case] is_empty: bool,
    ) {
        let container = container.into();
        match container.ion_type() {
            IonType::List | IonType::SExp => {
                // check length for given sequence value
                assert_eq!(container.as_sequence().unwrap().is_empty(), is_empty);
            }
            IonType::Struct => {
                // check length for given struct value
                assert_eq!(container.as_struct().unwrap().is_empty(), is_empty);
            }
            _ => {
                unreachable!("This test is only for container type elements")
            }
        }
    }

    #[test]
    fn list_display_roundtrip() {
        let list = ion_list![1, 2, 3, true, false];

        // Use the Display impl to serialize the list to text
        let text_list = format!("{list}");
        // Parse the result and make sure it represents the same data
        let expected_element: Element = list.into();
        let actual_element = Element::read_one(text_list).unwrap();
        assert!(expected_element.ion_eq(&actual_element));
    }

    #[test]
    fn sexp_display_roundtrip() {
        let sexp = ion_sexp! (1 2 3 true false);

        // Use the Display impl to serialize the sexp to text
        let text_sexp = format!("{sexp}");
        // Parse the result and make sure it represents the same data
        let expected_element: Element = sexp.into();
        let actual_element = Element::read_one(text_sexp).unwrap();
        assert!(expected_element.ion_eq(&actual_element));
    }

    #[test]
    fn struct_display_roundtrip() {
        let struct_ = ion_struct! {"foo": 1, "bar": 2, "baz": ion_list! [true, false]};

        // Use the Display impl to serialize the struct to text
        let text_struct = format!("{struct_}");
        // Parse the result and make sure it represents the same data
        let expected_element: Element = struct_.into();
        let actual_element = Element::read_one(text_struct).unwrap();
        assert!(expected_element.ion_eq(&actual_element));
    }
}