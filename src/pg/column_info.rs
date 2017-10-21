use r2d2;
use r2d2_postgres;
use r2d2_postgres::TlsMode;
use database::Database;
use dao::{Value};
use error::DbError;
use dao::Rows;
use dao;
use postgres;
use postgres::types::{self,ToSql,FromSql,Type};
use error::PlatformError;
use postgres::types::IsNull;
use std::error::Error;
use std::fmt;
use bigdecimal::BigDecimal;
use dao::TableName;
use dao::ColumnName;
use dao::FromDao;
use entity::EntityManager;
use column::{Column, ColumnConstraint, Literal, ColumnSpecification, Capacity};
use table::Table;
use types::SqlType;
use uuid::Uuid;

/// get all the columns of the table
pub fn get_columns(em: &EntityManager, table_name: &TableName) -> Result<Vec<Column>, DbError> {

    /// column name and comment
    #[derive(Debug, FromDao)]
    struct ColumnSimple{
        number: i32,
        name: String,
        comment: Option<String>,
    }

    impl ColumnSimple{
        fn to_column(&self, specification: ColumnSpecification) -> Column {
            Column{
                table: None,
                name: ColumnName::from(&self.name),
                comment: self.comment.to_owned(),
                specification: specification,             
            }
        }

    }
    let sql = "SELECT \
                 pg_attribute.attnum AS number, \
                 pg_attribute.attname AS name, \
                 pg_description.description AS comment \
            FROM pg_attribute \
       LEFT JOIN pg_class \
              ON pg_class.oid = pg_attribute.attrelid \
       LEFT JOIN pg_namespace \
              ON pg_namespace.oid = pg_class.relnamespace \
       LEFT JOIN pg_description \
              ON pg_description.objoid = pg_class.oid \
             AND pg_description.objsubid = pg_attribute.attnum \
           WHERE
                 pg_class.relname = $1 \
             AND pg_namespace.nspname = $2 \
             AND pg_attribute.attnum > 0 \
             AND pg_attribute.attisdropped = false \
        ORDER BY number\
    ";
    let schema = match table_name.schema {
        Some(ref schema) => schema.to_string(),
        None => "public".to_string()
    };
    println!("sql: {}", sql);
    let columns_simple: Result<Vec<ColumnSimple>, DbError> = 
        em.execute_sql_with_return(&sql, &[&table_name.name, &schema]);

    match columns_simple{
        Ok(columns_simple) => {
            let mut columns = vec![];
            for column_simple in columns_simple{
                let specification = get_column_specification(em, table_name, &column_simple.name);
                match specification{
                    Ok(specification) => {
                        let column = column_simple.to_column(specification);
                        columns.push(column);
                    },
                    // early return
                    Err(e) => {return Err(e);},
                }
            }
            Ok(columns)
        },
        Err(e) => Err(e),
    }
}


/// get the contrainst of each of this column
fn get_column_specification(em: &EntityManager, table_name: &TableName, column_name: &String)
    -> Result<ColumnSpecification, DbError> {

    /// null, datatype default value
    #[derive(Debug, FromDao)]
    struct ColumnConstraintSimple{
        not_null: bool,
        data_type: String,
        default: Option<String>,
    }

    impl ColumnConstraintSimple{


        fn to_column_specification(&self) -> ColumnSpecification {
            let (sql_type, capacity) = self.get_sql_type_capacity();
            println!("sql type: {:?} capacity: {:?}", sql_type, capacity);
            ColumnSpecification{
                 sql_type: sql_type, 
                 capacity: capacity,
                 constraints: self.to_column_constraints(),
            }
        }

        fn to_column_constraints(&self) -> Vec<ColumnConstraint> {
            let (sql_type, _) = self.get_sql_type_capacity();
            let mut constraints = vec![];
            if self.not_null{
                constraints.push(ColumnConstraint::NotNull);
            }
            if let Some(ref default) = self.default{
                let constraint = if default == "null" {
                    ColumnConstraint::DefaultValue(Literal::Null)
                }
                else if default.starts_with("nextval"){
                    ColumnConstraint::AutoIncrement
                }
                else {
                    let literal =  match sql_type {
                        SqlType::Bool => {
                            let v: bool = default.parse().unwrap();
                            Literal::Bool(v)
                        }
                        SqlType::Int 
                            | SqlType::Smallint 
                            | SqlType::Tinyint 
                            | SqlType::Bigint => {
                                let v: Result<i64,_> = default.parse();
                                match v{
                                    Ok(v) => Literal::Integer(v),
                                    Err(e) => panic!("error parsing to integer: {} error: {}", default, e)
                                }
                            },
                        SqlType::Float
                            | SqlType::Double
                            | SqlType::Numeric => {
                                let v: Result<f64,_> = default.parse();
                                match v{
                                    Ok(v) => Literal::Double(v),
                                    Err(e) => panic!("error parsing to f64: {} error: {}", default,
                                                    e)
                                }

                            }
                        SqlType::Uuid => {
                            if default == "uuid_generate_v4()"{
                               Literal::UuidGenerateV4
                            }
                            else{
                                let v: Result<Uuid,_> = Uuid::parse_str(default);
                                match v{
                                    Ok(v) => Literal::Uuid(v),
                                    Err(e) => panic!("error parsing to uuid: {} error: {}", default, e)
                                }
                            }
                        }
                        SqlType::Timestamp
                            | SqlType::TimestampTz
                            => {
                                if default == "now()" {
                                    Literal::CurrentTimestamp
                                }
                                else{
                                    panic!("timestamp other than now is not covered")
                                }
                            }
                        SqlType::Date => {
                            if default == "today()" {
                                Literal::CurrentDate
                            }else{
                                panic!("date other than today is not covered")
                            }
                        }
                        SqlType::Varchar 
                            | SqlType::Char
                            | SqlType::Tinytext
                            | SqlType::Mediumtext
                            | SqlType::Text
                                => Literal::String(default.to_owned()),
                        SqlType::Custom(s) => Literal::String(default.to_owned()),
                        _ => panic!("not convered: {:?}", sql_type),
                    };
                    ColumnConstraint::DefaultValue(literal)
                };
                constraints.push(constraint);
                
            }
            constraints
        }

        fn get_sql_type_capacity(&self) -> (SqlType, Option<Capacity>) {
            let data_type: &str = &self.data_type;
            println!("data_type: {}", data_type);
            let start = data_type.find('(');
            let end = data_type.find(')');
            let (dtype, capacity) = if let Some(start) = start {
                if let Some(end) = end {
                    let dtype = &data_type[0..start];
                    let range = &data_type[start+1..end];
                    let capacity = if range.contains(","){
                        let splinters = range.split(",").collect::<Vec<&str>>();
                        assert!(splinters.len() == 2, "There should only be 2 parts");
                        let r1:i32 = splinters[0].parse().unwrap();
                        let r2:i32= splinters[1].parse().unwrap();
                        Capacity::Range(r1,r2)
                    }
                    else{
                        let limit:i32 = range.parse().unwrap();
                        Capacity::Limit(limit)
                    };
                    println!("data_type: {}", dtype);
                    println!("range: {}", range);
                    (dtype, Some(capacity))
                }else{
                    (data_type, None)
                }
            }
            else{
                (data_type, None)
            };

            let sql_type = match dtype{
                "boolean" => SqlType::Bool,
                "tinyint" => SqlType::Tinyint,
                "smallint" | "year" => SqlType::Smallint,
                "int" | "integer" => SqlType::Int,
                "bigint" => SqlType::Bigint,
                "smallserial" => SqlType::SmallSerial,
                "serial" => SqlType::Serial,
                "bigserial" => SqlType::BigSerial,
                "real" => SqlType::Real,
                "float" => SqlType::Float,
                "double" => SqlType::Double,
                "numeric" => SqlType::Numeric,
                "tinyblob" => SqlType::Tinyblob,
                "mediumblob" => SqlType::Mediumblob,
                "blob" => SqlType::Blob,
                "longblob" => SqlType::Longblob,
                "varbinary" => SqlType::Varbinary,
                "char" => SqlType::Char,
                "varchar" | "character varying" => SqlType::Varchar,
                "tinytext" => SqlType::Tinytext,
                "mediumtext" => SqlType::Mediumtext,
                "text" => SqlType::Text,
                "text[]" => SqlType::TextArray,
                "uuid" => SqlType::Uuid,
                "date" => SqlType::Date,
                "timestamp" | "timestamp without time zone" => SqlType::Timestamp,
                "timestamp with time zone" => SqlType::TimestampTz,
                _ => SqlType::Custom(data_type.to_owned()), 
            };
            (sql_type, capacity)
        }

    }

    let sql = "SELECT DISTINCT \
               pg_attribute.attnotnull AS not_null, \
               pg_catalog.format_type(pg_attribute.atttypid, pg_attribute.atttypmod) AS data_type, \
     CASE WHEN pg_attribute.atthasdef THEN pg_attrdef.adsrc \
           END AS default \
          FROM pg_attribute \
          JOIN pg_class \
            ON pg_class.oid = pg_attribute.attrelid \
          JOIN pg_type \
            ON pg_type.oid = pg_attribute.atttypid \
     LEFT JOIN pg_attrdef \
            ON pg_attrdef.adrelid = pg_class.oid \
           AND pg_attrdef.adnum = pg_attribute.attnum \
     LEFT JOIN pg_namespace \
            ON pg_namespace.oid = pg_class.relnamespace \
     LEFT JOIN pg_constraint \
            ON pg_constraint.conrelid = pg_class.oid \
           AND pg_attribute.attnum = ANY (pg_constraint.conkey) \
         WHERE 
               pg_attribute.attname = $1 \
           AND pg_class.relname = $2 \
           AND pg_namespace.nspname = $3 \
           AND pg_attribute.attisdropped = false\
    ";
    let schema = match table_name.schema {
        Some(ref schema) => schema.to_string(),
        None => "public".to_string()
    };
    //println!("sql: {} column_name: {}, table_name: {}", sql, column_name, table_name.name);
    let column_constraint: Result<ColumnConstraintSimple, DbError> = 
        em.execute_sql_with_one_return(&sql, &[&column_name, &table_name.name, &schema]);
    column_constraint
        .map(|c| c.to_column_specification() )
}




#[cfg(test)]
mod test{

    use super::*;
    use pool::Pool;


    #[test]
    fn column_specification_for_actor_id(){
        let db_url = "postgres://postgres:p0stgr3s@localhost:5432/sakila";
        let mut pool = Pool::new();
        let em = pool.em(db_url);
        assert!(em.is_ok());
        let em = em.unwrap();
        let actor_table = TableName::from("actor");
        let actor_id_column = ColumnName::from("actor_id");
        let specification = get_column_specification(&em, &actor_table, &actor_id_column.name);
        println!("specification: {:#?}", specification);
        assert!(specification.is_ok());
        let specification = specification.unwrap();
        assert_eq!(specification, ColumnSpecification{
                           sql_type: SqlType::Int,
                           capacity: None,
                           constraints: vec![ColumnConstraint::NotNull,
                           ColumnConstraint::AutoIncrement],
                       });

    }
    #[test]
    fn column_specification_for_actor_last_updated(){
        let db_url = "postgres://postgres:p0stgr3s@localhost:5432/sakila";
        let mut pool = Pool::new();
        let em = pool.em(db_url);
        assert!(em.is_ok());
        let em = em.unwrap();
        let actor_table = TableName::from("actor");
        let column = ColumnName::from("last_update");
        let specification = get_column_specification(&em, &actor_table, &column.name);
        println!("specification: {:#?}", specification);
        assert!(specification.is_ok());
        let specification = specification.unwrap();
        assert_eq!(specification, ColumnSpecification{
                           sql_type: SqlType::Timestamp,
                           capacity: None,
                           constraints: vec![ColumnConstraint::NotNull,
                           ColumnConstraint::DefaultValue(Literal::CurrentTimestamp)],
                       });
    }

    #[test]
    fn column_for_actor(){
        let db_url = "postgres://postgres:p0stgr3s@localhost:5432/sakila";
        let mut pool = Pool::new();
        let em = pool.em(db_url);
        assert!(em.is_ok());
        let em = em.unwrap();
        let actor_table = TableName::from("actor");
        let columns = get_columns(&em, &actor_table);
        println!("columns: {:#?}", columns);
        assert!(columns.is_ok());
        let columns = columns.unwrap();
        assert_eq!(columns.len(), 4);
        assert_eq!(columns[1], 
                   Column{
                       table: None,
                       name: ColumnName::from("first_name"),
                       comment: None,
                       specification: ColumnSpecification{
                           sql_type: SqlType::Varchar,
                           capacity: Some(Capacity::Limit(45)),
                           constraints: vec![ColumnConstraint::NotNull],
                       }
                    });
    }

    #[test]
    fn column_for_film(){
        let db_url = "postgres://postgres:p0stgr3s@localhost:5432/sakila";
        let mut pool = Pool::new();
        let em = pool.em(db_url);
        assert!(em.is_ok());
        let em = em.unwrap();
        let table = TableName::from("film");
        let columns = get_columns(&em, &table);
        println!("columns: {:#?}", columns);
        assert!(columns.is_ok());
        let columns = columns.unwrap();
        assert_eq!(columns.len(), 14);
        assert_eq!(columns[7], 
                   Column{
                       table: None,
                       name: ColumnName::from("rental_rate"),
                       comment: None,
                       specification: ColumnSpecification{
                           sql_type: SqlType::Numeric,
                           capacity: Some(Capacity::Range(4,2)),
                           constraints: vec![ColumnConstraint::NotNull,
                                    ColumnConstraint::DefaultValue(Literal::Double(4.99))
                                ],
                       }
                    });
    }
}
