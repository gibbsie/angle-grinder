use crate::data;
use crate::errors::SyntaxErrors;
use lazy_static::lazy_static;
use nom;
use nom::types::CompleteStr;
use nom::*;
use nom::{digit1, double, is_alphabetic, is_alphanumeric, is_digit, multispace};
use nom_locate::LocatedSpan;
use std::convert::From;
use std::str;

/// Wraps the result of the child parser in a Positioned and sets the start_pos and end_pos
/// accordingly.
macro_rules! with_pos {
  ($i:expr, $submac:ident!( $($args:tt)* )) => ({
      // XXX The ws!() combinator does not mix well with custom combinators since it does some
      // rewriting, but only for things it knows about.  So, we put the ws!() combinator inside
      // calls to with_pos!() and have with_pos!() eat up any initial space with space0().
      match space0($i) {
          Err(e) => Err(e),
          Ok((i1, _o)) => {
              let start_pos: QueryPosition = i1.into();
              match $submac!(i1, $($args)*) {
                  Ok((i, o)) => Ok((i, Positioned {
                      start_pos,
                      value: o,
                      end_pos: i.into(),
                  })),
                  Err(e) => Err(e),
              }
          }
      }
  });
  ($i:expr, $f:expr) => (
    with_pos!($i, call!($f));
  );
}

/// Dynamic version of `alt` that takes a slice of strings
fn alternative<T>(input: T, alternatives: &[&'static str]) -> IResult<T, T>
where
    T: InputTake,
    T: Compare<&'static str>,
    T: InputLength,
    T: AtEof,
    T: Clone,
{
    let mut last_err = None;
    for alternative in alternatives {
        let inp = input.clone();
        match tag!(inp, &**alternative) {
            done @ Ok(..) => return done,
            err @ Err(..) => last_err = Some(err), // continue
        }
    }
    last_err.unwrap()
}

pub const VALID_AGGREGATES: &'static [&str] = &[
    "count",
    "average",
    "avg",
    "average",
    "sum",
    "count_distinct",
    "sort",
];

pub const VALID_INLINE: &'static [&str] = &["parse", "limit", "json", "total", "fields", "where"];

lazy_static! {
    pub static ref VALID_OPERATORS: Vec<&'static str> =
        { [VALID_INLINE, VALID_AGGREGATES].concat() };
}

/// Type used to track the current fragment being parsed and its location in the original input.
pub type Span<'a> = LocatedSpan<CompleteStr<'a>>;

/// Container for the position of some syntax in the input string.  This is similar to the Span,
/// but it only contains the offset.
#[derive(Debug, PartialEq, Clone)]
pub struct QueryPosition(pub usize);

impl<'a> From<Span<'a>> for QueryPosition {
    fn from(located_span: Span<'a>) -> Self {
        QueryPosition(located_span.offset)
    }
}

/// Container for values from the query that records the location in the query string.
#[derive(Debug, PartialEq, Clone)]
pub struct Positioned<T> {
    pub start_pos: QueryPosition,
    pub end_pos: QueryPosition,
    pub value: T,
}

impl<T> Positioned<T> {
    pub fn into(&self) -> &T {
        &self.value
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ComparisonOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum BinaryOp {
    Comparison(ComparisonOp),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum UnaryOp {
    Not,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Expr {
    Column(String),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Value(data::Value),
}

/// The KeywordType determines how a keyword string should be interpreted.
#[derive(Debug, PartialEq, Eq, Clone)]
enum KeywordType {
    /// The keyword string should exactly match the input.
    EXACT,
    /// The keyword string can contain wildcards.
    WILDCARD,
}

/// Represents a `keyword` search string.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Keyword(String, KeywordType);

impl Keyword {
    /// Create a Keyword that will exactly match an input string.
    pub fn new_exact(str: String) -> Keyword {
        Keyword(str, KeywordType::EXACT)
    }

    /// Create a Keyword that can contain wildcards
    pub fn new_wildcard(str: String) -> Keyword {
        Keyword(str, KeywordType::WILDCARD)
    }

    /// Test if this is an empty keyword string
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Convert this keyword to a `regex::Regex` object.
    pub fn to_regex(&self) -> regex::Regex {
        let mut regex_str = regex::escape(&self.0.replace("\\\"", "\""));

        regex_str.insert_str(0, "(?i)");
        if self.1 == KeywordType::WILDCARD {
            regex_str = regex_str.replace("\\*", "(.*?)");
            // If it ends with a star, we need to ensure we read until the end.
            if self.0.ends_with('*') {
                regex_str.push('$');
            }
        }

        regex::Regex::new(&regex_str).unwrap()
    }
}

#[derive(Debug, PartialEq)]
pub enum Operator {
    Inline(Positioned<InlineOperator>),
    MultiAggregate(MultiAggregateOperator),
    Sort(SortOperator),
}

#[derive(Debug, PartialEq, Clone)]
pub enum InlineOperator {
    Json {
        input_column: Option<String>,
    },
    Parse {
        pattern: Keyword,
        fields: Vec<String>,
        input_column: Option<Expr>,
        no_drop: bool,
    },
    Fields {
        mode: FieldMode,
        fields: Vec<String>,
    },
    Where {
        expr: Option<Positioned<Expr>>,
    },
    Limit {
        /// The count for the limit is pretty loosely typed at this point, the next phase will
        /// check the value to see if it's sane or provide a default if no number was given.
        count: Option<Positioned<f64>>,
    },
    Total {
        input_column: Expr,
        output_column: String,
    },
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FieldMode {
    Only,
    Except,
}

#[derive(Debug, PartialEq)]
pub enum SortMode {
    Ascending,
    Descending,
}

#[derive(Debug, PartialEq)]
pub enum AggregateFunction {
    Count,
    Sum {
        column: Expr,
    },
    Average {
        column: Expr,
    },
    Percentile {
        percentile: f64,
        percentile_str: String,
        column: Expr,
    },
    CountDistinct {
        column: Option<Positioned<Vec<Expr>>>,
    },
}

#[derive(Debug, PartialEq)]
pub struct MultiAggregateOperator {
    pub key_cols: Vec<Expr>,
    pub key_col_headers: Vec<String>,
    pub aggregate_functions: Vec<(String, Positioned<AggregateFunction>)>,
}

#[derive(Debug, PartialEq)]
pub struct SortOperator {
    pub sort_cols: Vec<String>,
    pub direction: SortMode,
}

#[derive(Debug, PartialEq)]
pub struct Query {
    pub search: Vec<Keyword>,
    pub operators: Vec<Operator>,
}

fn is_ident(c: char) -> bool {
    is_alphanumeric(c as u8) || c == '_'
}

fn starts_ident(c: char) -> bool {
    is_alphabetic(c as u8) || c == '_'
}

/// Tests if the input character can be part of a search keyword.
///
/// Based on the SumoLogic keyword syntax:
///
/// https://help.sumologic.com/05Search/Get-Started-with-Search/How-to-Build-a-Search/Keyword-Search-Expressions
fn is_keyword(c: char) -> bool {
    match c {
        '-' | '_' | ':' | '/' | '.' | '+' | '@' | '#' | '$' | '%' | '^' | '*' => true,
        alpha if is_alphanumeric(alpha as u8) => true,
        _ => false,
    }
}

fn not_escape_sq(c: char) -> bool {
    c != '\\' && c != '\''
}

fn not_escape_dq(c: char) -> bool {
    c != '\\' && c != '\"'
}
named!(value<Span, data::Value>, ws!(
    alt!(
        map!(quoted_string, |s|data::Value::Str(s.to_string()))
        | map!(digit1, |s|data::Value::from_string(s.fragment.0))
    )
));
named!(ident<Span, String>, do_parse!(
    start: take_while1!(starts_ident) >>
    rest: take_while!(is_ident) >>
    (start.fragment.0.to_owned() + rest.fragment.0)
));

named!(e_ident<Span, Expr>,
    ws!(alt!(
      map!(ident, |col|Expr::Column(col.to_owned()))
    | map!(value, Expr::Value)
      //expr
    | ws!(add_return_error!(SyntaxErrors::StartOfError.into(), delimited!(
          tag!("("),
          expr,
          return_error!(SyntaxErrors::MissingParen.into(), tag!(")")))))
)));

named!(keyword<Span, String>, do_parse!(
    start: take_while1!(is_keyword) >>
    rest: take_while!(is_keyword) >>
    (start.fragment.0.to_owned() + rest.fragment.0)
));

named!(comp_op<Span, ComparisonOp>, ws!(alt!(
    map!(tag!("=="), |_|ComparisonOp::Eq)
    | map!(tag!("<="), |_|ComparisonOp::Lte)
    | map!(tag!(">="), |_|ComparisonOp::Gte)
    | map!(tag!("!="), |_|ComparisonOp::Neq)
    | map!(tag!(">"), |_|ComparisonOp::Gt)
    | map!(tag!("<"), |_|ComparisonOp::Lt)
)));

named!(unary_op<Span, UnaryOp>, ws!(alt!(
    map!(tag!("!"), |_|UnaryOp::Not)
)));

named!(expr<Span, Expr>, ws!(alt!(
    do_parse!(
        l: e_ident >>
        comp: comp_op >>
        r: e_ident >>
        ( Expr::Binary { op: BinaryOp::Comparison(comp), left: Box::new(l), right: Box::new(r)} )
    )
    | do_parse!(
        op: unary_op >>
        operand: e_ident >>
        ( Expr::Unary { op, operand: Box::new(operand) } )
    )
    | e_ident
)));

named!(json<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("json") >>
    from_column_opt: opt!(ws!(preceded!(tag!("from"), ident))) >>
    (InlineOperator::Json { input_column: from_column_opt.map(|s|s.to_string()) })
))));

named!(whre<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("where") >>
    ex: opt!(with_pos!(expr)) >>
    (InlineOperator::Where { expr: ex })
))));

named!(limit<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("limit") >>
    count: opt!(with_pos!(double)) >>
    (InlineOperator::Limit{
        count
    })
))));

named!(total<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("total") >>
    input_column: delimited!(tag!("("), expr, tag!(")")) >>
    rename_opt: opt!(ws!(preceded!(tag!("as"), ident))) >>
    (InlineOperator::Total{
        input_column,
        output_column:
            rename_opt.map(|s|s.to_string()).unwrap_or_else(||"_total".to_string()),
})))));

named!(double_quoted_string <Span, &str>, add_return_error!(
    SyntaxErrors::StartOfError.into(), delimited!(
        tag!("\""),
        map!(escaped!(take_while1!(not_escape_dq), '\\', anychar), |ref s|s.fragment.0),
        return_error!(SyntaxErrors::UnterminatedDoubleQuotedString.into(), tag!("\""))
)));

named!(single_quoted_string <Span, &str>, add_return_error!(
    SyntaxErrors::StartOfError.into(), delimited!(
        tag!("'"),
        map!(escaped!(take_while1!(not_escape_sq), '\\', anychar), |ref s|s.fragment.0),
        return_error!(SyntaxErrors::UnterminatedSingleQuotedString.into(), tag!("'"))
)));

named!(quoted_string<Span, &str>, alt!(double_quoted_string | single_quoted_string));

named!(var_list<Span, Vec<String> >, ws!(separated_nonempty_list!(
    tag!(","), ws!(ident)
)));

named!(sourced_expr_list<Span, Vec<(String, Expr)> >, ws!(separated_nonempty_list!(
    tag!(","), ws!(sourced_expr)
)));

named!(sourced_expr<Span, (String, Expr)>, ws!(
    do_parse!(
        ex: recognize!(expr) >>
        (
            (ex.fragment.0.trim().to_string(), expr(ex).unwrap().1)
        )
)));

named_args!(did_you_mean<'a>(choices: &[&'static str], err: SyntaxErrors)<Span<'a>, Span<'a>>,
        preceded!(space0,
            return_error!(SyntaxErrors::StartOfError.into(),
                alt!(
                    // Either we find a valid operator name
                    // To prevent a prefix from partially matching, require `not!(alpha1)` after the tag is consumed
                    terminated!(alt!(pct_fn | call!(alternative, choices)), not!(alpha1)) |

                    // Or we return an error after consuming a word
                    // If we exhaust all other possibilities, consume a word and return an error. We won't
                    // find `tag!("a")` after an identifier, so that's a guaranteed to fail and produce `not an operator`
                    terminated!(take_while!(is_ident), return_error!(err.clone().into(), tag!("a")))))
        )
);

named!(did_you_mean_operator<Span, Span>,
    call!(did_you_mean, &VALID_OPERATORS, SyntaxErrors::NotAnOperator)
);

named!(did_you_mean_aggregate<Span, Span>,
    call!(did_you_mean, &VALID_AGGREGATES, SyntaxErrors::NotAnAggregateOperator)
);

// parse "blah * ... *" [from other_field] as x, y
named!(parse<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("parse") >>
    pattern: quoted_string >>
    from_column_opt: opt!(ws!(preceded!(tag!("from"), expr))) >>
    tag!("as") >>
    vars: var_list >>
    no_drop_opt: opt!(ws!(tag!("nodrop"))) >>
    ( InlineOperator::Parse{
        pattern: Keyword::new_wildcard(pattern.to_string()),
        fields: vars,
        input_column: from_column_opt,
        no_drop: no_drop_opt.is_some()
        } )
))));

named!(fields_mode<Span, FieldMode>, alt!(
    map!(
        alt!(tag!("+") | tag!("only") | tag!("include")),
        |_|FieldMode::Only
    ) |
    map!(
        alt!(tag!("-") | tag!("except") | tag!("drop")),
        |_|FieldMode::Except
    )
));

named!(fields<Span, Positioned<InlineOperator>>, with_pos!(ws!(do_parse!(
    tag!("fields") >>
    mode: opt!(fields_mode) >>
    fields: var_list >>
    (
        InlineOperator::Fields {
            mode: mode.unwrap_or(FieldMode::Only),
            fields
        }
    )
))));

named!(arg_list<Span, Positioned<Vec<Expr>>>, add_return_error!(
    SyntaxErrors::StartOfError.into(), with_pos!(delimited!(
        tag!("("),
        ws!(separated_list!(tag!(","), ws!(expr))),
        return_error!(SyntaxErrors::MissingParen.into(), tag!(")"))))
));

named!(count<Span, Positioned<AggregateFunction>>, with_pos!(map!(tag!("count"),
    |_s|AggregateFunction::Count{}))
);

named!(average<Span, Positioned<AggregateFunction>>, with_pos!(ws!(do_parse!(
    alt!(tag!("avg") | tag!("average")) >>
    column: delimited!(tag!("("), expr ,tag!(")")) >>
    (AggregateFunction::Average{column})
))));

named!(count_distinct<Span, Positioned<AggregateFunction>>, with_pos!(ws!(do_parse!(
    tag!("count_distinct") >>
    column: opt!(arg_list) >>
    (AggregateFunction::CountDistinct{ column })
))));

named!(sum<Span, Positioned<AggregateFunction>>, with_pos!(ws!(do_parse!(
    tag!("sum") >>
    column: delimited!(tag!("("), expr,tag!(")")) >>
    (AggregateFunction::Sum{column})
))));

fn is_digit_char(digit: char) -> bool {
    is_digit(digit as u8)
}

named!(pct_fn<Span, Span>, preceded!(
    alt!(tag!("pct") | tag!("percentile") | tag!("p")),
    take_while_m_n!(2, 2, is_digit_char)
));

named!(p_nn<Span, Positioned<AggregateFunction>>, ws!(
    with_pos!(do_parse!(
        pct: pct_fn >>
        column: delimited!(tag!("("), expr,tag!(")")) >>
        (AggregateFunction::Percentile{
            column,
            percentile: (".".to_owned() + pct.fragment.0).parse::<f64>().unwrap(),
            percentile_str: pct.fragment.0.to_string()
        })
    ))
));

named!(inline_operator<Span, Operator>,
    map!(alt!(parse | json | fields | whre | limit | total), Operator::Inline)
);

named!(aggregate_function<Span, Positioned<AggregateFunction>>, do_parse!(
    peek!(did_you_mean_aggregate) >>
    res: alt_complete!(
        count_distinct |
        count |
        average |
        sum |
        p_nn) >> (res)
));

named!(operator<Span, Operator>, do_parse!(
    peek!(did_you_mean_operator) >>
    res: alt_complete!(inline_operator | sort | multi_aggregate_operator) >> (res)
));

// count by x,y
// avg(foo) by x

fn default_output(func: &Positioned<AggregateFunction>) -> String {
    match func.into() {
        AggregateFunction::Count { .. } => "_count".to_string(),
        AggregateFunction::Sum { .. } => "_sum".to_string(),
        AggregateFunction::Average { .. } => "_average".to_string(),
        AggregateFunction::CountDistinct { .. } => "_countDistinct".to_string(),
        AggregateFunction::Percentile {
            ref percentile_str, ..
        } => "p".to_string() + percentile_str,
    }
}

named!(complete_agg_function<Span, (String, Positioned<AggregateFunction>)>, ws!(do_parse!(
        agg_function: aggregate_function >>
        rename_opt: opt!(ws!(preceded!(tag!("as"), ident))) >>
        (
            rename_opt.map(|s|s.to_string()).unwrap_or_else(||default_output(&agg_function)),
            agg_function
        )
    ))
);

named!(multi_aggregate_operator<Span, Operator>, ws!(do_parse!(
    agg_functions: ws!(separated_nonempty_list!(tag!(","), complete_agg_function)) >>
    key_cols_opt: opt!(preceded!(tag!("by"), sourced_expr_list)) >>
    (Operator::MultiAggregate(MultiAggregateOperator {
        key_col_headers: key_cols_opt.clone()
            .unwrap_or_default()
            .iter().cloned().map(|col|col.0).collect(),
        key_cols: key_cols_opt.clone()
            .unwrap_or_default()
            .iter().cloned().map(|col|col.1).collect(),
        aggregate_functions: agg_functions,
     })))
));

named!(sort_mode<Span, SortMode>, alt!(
    map!(
        alt!(tag!("asc") | tag!("ascending")),
        |_|SortMode::Ascending
    ) |
    map!(
        alt!(tag!("desc") | tag!("dsc") | tag!("descending")),
        |_|SortMode::Descending
    )
));

named!(sort<Span, Operator>, ws!(do_parse!(
    tag!("sort") >>
    key_cols_opt: opt!(preceded!(opt!(tag!("by")), var_list)) >>
    dir: opt!(sort_mode) >>
    (Operator::Sort(SortOperator{
        sort_cols: key_cols_opt.unwrap_or_default(),
        direction: dir.unwrap_or(SortMode::Ascending) ,
     })))
));

named!(filter_cond<Span, Keyword>, alt!(
    map!(quoted_string, |s| Keyword::new_exact(s.to_string())) |
    map!(keyword, |s| Keyword::new_wildcard(s.trim_matches('*').to_string()))
));

named!(filter<Span, Vec<Keyword>>, map!(
    separated_nonempty_list!(multispace, filter_cond),
    // An empty keyword would match everything, so there's no reason to
    |mut v| {
        v.retain(|k| !k.is_empty());
        v
    }
));

named!(pub query<Span, Query, SyntaxErrors>, fix_error!(SyntaxErrors, exact!(ws!(do_parse!(
    filter: filter >>
    operators: opt!(preceded!(tag!("|"), ws!(separated_nonempty_list!(tag!("|"), operator)))) >>
    (Query{
        search: filter,
        operators: operators.unwrap_or_default()
    }))
))));

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! expect {
        ($f:expr, $inp:expr, $res:expr) => {{
            let parse_result = $f(Span::new(CompleteStr($inp)));
            match parse_result {
                Ok((
                    LocatedSpan {
                        fragment: leftover, ..
                    },
                    actual_result,
                )) => {
                    assert_eq!(actual_result, $res);
                    assert_eq!(leftover, CompleteStr(""));
                }
                Err(e) => panic!(format!(
                    "Parse failed, but was expected to succeed: \n{:?}",
                    e
                )),
            }
        }};
    }

    macro_rules! expect_fail {
        ($f:expr, $inp:expr) => {{
            let parse_result = $f(Span::new(CompleteStr($inp)));
            match parse_result {
                Ok(_res) => panic!(format!("Expected parse to fail, but it succeeded")),
                // TODO: enable assertions of specific errors
                Err(_e) => (),
            }
        }};
    }

    #[test]
    fn parse_keyword_string() {
        expect!(keyword, "abc", "abc".to_string());
        expect!(keyword, "one-two-three", "one-two-three".to_string());
    }

    #[test]
    fn parse_quoted_string() {
        expect!(quoted_string, "\"hello\"", "hello");
        expect!(quoted_string, "'hello'", "hello");
        expect!(quoted_string, r#""test = [*=*] * ""#, "test = [*=*] * ");
        expect_fail!(quoted_string, "\"hello'");
    }

    #[test]
    fn parse_expr() {
        expect!(
            expr,
            "a == b",
            Expr::Binary {
                op: BinaryOp::Comparison(ComparisonOp::Eq),
                left: Box::new(Expr::Column("a".to_string())),
                right: Box::new(Expr::Column("b".to_string())),
            }
        );
    }

    #[test]
    fn parse_expr_value() {
        expect!(
            expr,
            "a <= \"b\"",
            Expr::Binary {
                op: BinaryOp::Comparison(ComparisonOp::Lte),
                left: Box::new(Expr::Column("a".to_string())),
                right: Box::new(Expr::Value(data::Value::Str("b".to_string()))),
            }
        );
    }

    #[test]
    fn parse_expr_ident() {
        expect!(expr, "foo", Expr::Column("foo".to_string()));
    }

    #[test]
    fn parse_ident() {
        expect!(ident, "hello123", "hello123".to_string());
        expect!(ident, "x", "x".to_string());
        expect!(ident, "_x", "_x".to_string());
        expect_fail!(ident, "5x");
    }

    #[test]
    fn parse_var_list() {
        expect!(
            var_list,
            "a, b, def, g_55",
            vec![
                "a".to_string(),
                "b".to_string(),
                "def".to_string(),
                "g_55".to_string(),
            ]
        );
    }

    #[test]
    fn parse_parses() {
        expect!(
            parse,
            r#"parse "[key=*]" as v"#,
            Positioned {
                start_pos: QueryPosition(0),
                end_pos: QueryPosition(20),
                value: InlineOperator::Parse {
                    pattern: Keyword::new_wildcard("[key=*]".to_string()),
                    fields: vec!["v".to_string()],
                    input_column: None,
                    no_drop: false
                }
            }
        );
        expect!(
            parse,
            r#"parse "[key=*]" as v nodrop"#,
            Positioned {
                start_pos: QueryPosition(0),
                end_pos: QueryPosition(27),
                value: InlineOperator::Parse {
                    pattern: Keyword::new_wildcard("[key=*]".to_string()),
                    fields: vec!["v".to_string()],
                    input_column: None,
                    no_drop: true
                }
            }
        );
        expect!(
            parse,
            r#"parse "[key=*][val=*]" as k,v nodrop"#,
            Positioned {
                start_pos: QueryPosition(0),
                end_pos: QueryPosition(36),
                value: InlineOperator::Parse {
                    pattern: Keyword::new_wildcard("[key=*][val=*]".to_string()),
                    fields: vec!["k".to_string(), "v".to_string()],
                    input_column: None,
                    no_drop: true
                }
            }
        );
    }

    #[test]
    fn parse_operator() {
        expect!(
            operator,
            "  json",
            Operator::Inline(Positioned {
                start_pos: QueryPosition(2),
                end_pos: QueryPosition(6),
                value: InlineOperator::Json { input_column: None }
            })
        );
        expect!(
            operator,
            r#" parse "[key=*]" from field as v "#,
            Operator::Inline(Positioned {
                start_pos: QueryPosition(1),
                end_pos: QueryPosition(33),
                value: InlineOperator::Parse {
                    pattern: Keyword::new_wildcard("[key=*]".to_string()),
                    fields: vec!["v".to_string()],
                    input_column: Some(Expr::Column("field".to_string())),
                    no_drop: false
                },
            })
        );
    }

    #[test]
    fn parse_limit() {
        expect!(
            operator,
            " limit",
            Operator::Inline(Positioned {
                start_pos: QueryPosition(1),
                end_pos: QueryPosition(6),
                value: InlineOperator::Limit { count: None }
            })
        );
        expect!(
            operator,
            " limit 5",
            Operator::Inline(Positioned {
                start_pos: QueryPosition(1),
                end_pos: QueryPosition(8),
                value: InlineOperator::Limit {
                    count: Some(Positioned {
                        value: 5.0,
                        start_pos: QueryPosition(7),
                        end_pos: QueryPosition(8)
                    })
                }
            })
        );
        expect!(
            operator,
            " limit -5",
            Operator::Inline(Positioned {
                start_pos: QueryPosition(1),
                end_pos: QueryPosition(9),
                value: InlineOperator::Limit {
                    count: Some(Positioned {
                        value: -5.0,
                        start_pos: QueryPosition(7),
                        end_pos: QueryPosition(9)
                    }),
                }
            })
        );
        expect!(
            operator,
            " limit 1e2",
            Operator::Inline(Positioned {
                start_pos: QueryPosition(1),
                end_pos: QueryPosition(10),
                value: InlineOperator::Limit {
                    count: Some(Positioned {
                        value: 1e2,
                        start_pos: QueryPosition(7),
                        end_pos: QueryPosition(10)
                    })
                }
            })
        );
    }

    #[test]
    fn parse_agg_operator() {
        expect!(
            multi_aggregate_operator,
            "count as renamed by x, y",
            Operator::MultiAggregate(MultiAggregateOperator {
                key_cols: vec![Expr::Column("x".to_string()), Expr::Column("y".to_string())],
                key_col_headers: vec!["x".to_string(), "y".to_string()],
                aggregate_functions: vec![(
                    "renamed".to_string(),
                    Positioned {
                        value: AggregateFunction::Count,
                        start_pos: QueryPosition(0),
                        end_pos: QueryPosition(5)
                    }
                )],
            })
        );
    }

    #[test]
    fn parse_percentile() {
        expect!(
            complete_agg_function,
            "p50(x)",
            (
                "p50".to_string(),
                Positioned {
                    value: AggregateFunction::Percentile {
                        column: Expr::Column("x".to_string()),
                        percentile: 0.5,
                        percentile_str: "50".to_string(),
                    },
                    start_pos: QueryPosition(0),
                    end_pos: QueryPosition(6),
                }
            )
        );
    }

    #[test]
    fn query_no_operators() {
        expect!(
            query,
            " * ",
            Query {
                search: vec![],
                operators: vec![],
            }
        );
        expect!(
            query,
            " filter ",
            Query {
                search: vec![Keyword::new_wildcard("filter".to_string())],
                operators: vec![],
            }
        );
        expect!(
            query,
            " *abc* ",
            Query {
                search: vec![Keyword::new_wildcard("abc".to_string())],
                operators: vec![],
            }
        );
        expect!(
            query,
            " abc def \"*ghi*\" ",
            Query {
                search: vec![
                    Keyword::new_wildcard("abc".to_string()),
                    Keyword::new_wildcard("def".to_string()),
                    Keyword::new_exact("*ghi*".to_string()),
                ],
                operators: vec![],
            }
        );
    }

    #[test]
    fn query_operators() {
        let query_str =
            r#"* | json from col | parse "!123*" as foo | count by foo, foo == 123 | sort by foo dsc "#;
        expect!(
            query,
            query_str,
            Query {
                search: vec![],
                operators: vec![
                    Operator::Inline(Positioned {
                        start_pos: QueryPosition(4),
                        end_pos: QueryPosition(18),
                        value: InlineOperator::Json {
                            input_column: Some("col".to_string()),
                        }
                    }),
                    Operator::Inline(Positioned {
                        start_pos: QueryPosition(20),
                        end_pos: QueryPosition(41),
                        value: InlineOperator::Parse {
                            pattern: Keyword::new_wildcard("!123*".to_string()),
                            fields: vec!["foo".to_string()],
                            input_column: None,
                            no_drop: false
                        }
                    }),
                    Operator::MultiAggregate(MultiAggregateOperator {
                        key_col_headers: vec!["foo".to_string(), "foo == 123".to_string()],
                        key_cols: vec![
                            Expr::Column("foo".to_string()),
                            Expr::Binary {
                                op: BinaryOp::Comparison(ComparisonOp::Eq),
                                left: Box::new(Expr::Column("foo".to_string())),
                                right: Box::new(Expr::Value(data::Value::Int(123))),
                            },
                        ],
                        aggregate_functions: vec![(
                            "_count".to_string(),
                            Positioned {
                                value: AggregateFunction::Count {},
                                start_pos: QueryPosition(43),
                                end_pos: QueryPosition(48),
                            }
                        ),],
                    }),
                    Operator::Sort(SortOperator {
                        sort_cols: vec!["foo".to_string()],
                        direction: SortMode::Descending,
                    }),
                ],
            }
        );
    }
}
