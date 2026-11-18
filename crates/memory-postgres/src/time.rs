use jiff::Timestamp;
use jiff::civil::Date;
use jiff_sqlx::ToSqlx as _;

pub(crate) trait BindTime {
    type Bound;

    fn bind_time(self) -> Self::Bound;
}

impl BindTime for Timestamp {
    type Bound = jiff_sqlx::Timestamp;

    fn bind_time(self) -> Self::Bound {
        self.to_sqlx()
    }
}

impl BindTime for Date {
    type Bound = jiff_sqlx::Date;

    fn bind_time(self) -> Self::Bound {
        self.to_sqlx()
    }
}

impl<T: BindTime> BindTime for Option<T> {
    type Bound = Option<T::Bound>;

    fn bind_time(self) -> Self::Bound {
        self.map(BindTime::bind_time)
    }
}

impl<T: BindTime + Copy> BindTime for &[T] {
    type Bound = Vec<T::Bound>;

    fn bind_time(self) -> Self::Bound {
        self.iter().copied().map(BindTime::bind_time).collect()
    }
}

#[derive(Debug)]
pub struct OptionalTimestamp(Option<Timestamp>);

impl From<OptionalTimestamp> for Option<Timestamp> {
    fn from(value: OptionalTimestamp) -> Self {
        value.0
    }
}

impl sqlx::Type<sqlx::Postgres> for OptionalTimestamp {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <jiff_sqlx::Timestamp as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for OptionalTimestamp {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let value = <Option<jiff_sqlx::Timestamp> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(value.map(jiff_sqlx::Timestamp::to_jiff)))
    }
}

pub use jiff_sqlx::Timestamp as PgTimestamp;
