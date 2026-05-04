//! SQLite 系 driver (GPKG / SpatiaLite) の reader を真のストリーミング化するための
//! 共通 iterator。
//!
//! # 設計
//!
//! `LayerReader::batches()` は `Box<dyn Iterator + Send + '_>` を返すため、各 driver は
//! 内部 field を借用して iterator を構築する。SQLite reader で素直に `Statement<'_>` /
//! `Rows<'_>` を struct field に保持しようとすると self-referential になって
//! コンパイルが通らない (`Statement` が `&Connection` を借用、`Rows` が `&mut Statement` を
//! 借用するため)。
//!
//! 本モジュールは self-referential を避けるため「**1 batch ごとに `prepare_cached` →
//! `query` → 全行 drain → drop**」の小さなスコープに `Statement` と `Rows` の lifetime を
//! 閉じ込める。`Connection` への借用は `next_batch()` 呼び出し中だけ生きるため、batch を
//! またいだ resource leak を起こさない。
//!
//! # 2 つのモード
//!
//! - [`KeysetRowsIter`] — `WHERE rowid > ? ORDER BY rowid LIMIT ?` の keyset pagination。
//!   GPKG / SpatiaLite の table モードで使う。OFFSET より大きいテーブルでスケールする。
//!   呼び出し側は SELECT の **末尾に rowid 列を追加** する SQL テンプレートを渡す慣習で、
//!   iterator は内部で末尾列を i64 として取り出して次イテレーションの cursor に使う。
//!   返却される [`RowBatch`] には rowid 列は含まれない。
//! - [`OffsetRowsIter`] — `LIMIT ? OFFSET ?` の素朴 pagination。SpatiaLite の `--query`
//!   モード (任意 SQL) では rowid を SELECT に追加できないため fallback として用いる。
//!   返却される [`RowBatch`] には SQL の全列が入る。

use rusqlite::{params, types::Value, Connection};
use shpx_core::{Error, Result};

/// 1 batch 分の行データ。`rows[i]` は SELECT の i 行目、`rows[i][j]` は j 列目の値。
///
/// keyset モードの場合、SQL 末尾の rowid 列は iterator が消費するため `rows` には
/// **含まれない** (列数 = SQL の列数 - 1)。offset モードの場合は SQL の全列が入る。
#[derive(Debug, Clone)]
pub struct RowBatch {
    pub rows: Vec<Vec<Value>>,
}

impl RowBatch {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// rowid keyset pagination iterator。
///
/// SQL テンプレートは `?1` に最終 rowid、`?2` に LIMIT を bind する形式で、SELECT の
/// **末尾列に rowid を含める** こと。例:
///
/// ```ignore
/// SELECT col1, col2, geom, rowid FROM mytable
///   WHERE rowid > ?1 ORDER BY rowid LIMIT ?2
/// ```
pub struct KeysetRowsIter<'a> {
    /// `&mut Connection` を取るのは `Send` を満たすため (`rusqlite::Connection` は `Send`
    /// だが `!Sync`、つまり `&Connection: !Send`)。`LayerReader::batches()` の戻り値が
    /// `+ Send` を要求するため、共有参照では `Box<dyn Iterator + Send>` に詰められない。
    /// 中身の `prepare_cached` は `&self` だけ取るので mut 借用は実質 unique 性のためだけ。
    conn: &'a mut Connection,
    sql: String,
    last_rowid: i64,
    batch_size: usize,
    driver_name: &'static str,
    done: bool,
}

impl<'a> KeysetRowsIter<'a> {
    /// `sql` は `?1` (last_rowid)、`?2` (LIMIT) を持ち、SELECT 末尾列が rowid である必要がある。
    /// `driver_name` は `Error::Driver` の name フィールドに使う。
    pub fn new(
        conn: &'a mut Connection,
        sql: String,
        batch_size: usize,
        driver_name: &'static str,
    ) -> Self {
        Self {
            conn,
            sql,
            last_rowid: 0,
            batch_size,
            driver_name,
            done: false,
        }
    }

    /// 次の batch を読み込む。空 batch (= 全件読了) は `Ok(None)` を返す。
    pub fn next_batch(&mut self) -> Result<Option<RowBatch>> {
        if self.done {
            return Ok(None);
        }
        let limit_i64 = i64::try_from(self.batch_size).map_err(|_| {
            Error::driver_msg(
                self.driver_name,
                format!("batch_size {} exceeds i64::MAX", self.batch_size),
            )
        })?;

        // クロージャで包んでエラー時に必ず done を立てる (失敗後 next_batch を再 SQL しない)。
        let result = self.execute_batch(limit_i64);
        if result.is_err() {
            self.done = true;
        }
        result
    }

    fn execute_batch(&mut self, limit_i64: i64) -> Result<Option<RowBatch>> {
        let mut stmt = self
            .conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::driver(self.driver_name, e))?;
        let n_cols = stmt.column_count();
        if n_cols < 1 {
            return Err(Error::driver_msg(
                self.driver_name,
                "KeysetRowsIter: SELECT must include at least the rowid column",
            ));
        }

        let mut rows = stmt
            .query(params![self.last_rowid, limit_i64])
            .map_err(|e| Error::driver(self.driver_name, e))?;

        let mut out: Vec<Vec<Value>> = Vec::with_capacity(self.batch_size);
        let mut last_rowid_in_batch = self.last_rowid;
        while let Some(row) = rows
            .next()
            .map_err(|e| Error::driver(self.driver_name, e))?
        {
            let attr_cols = n_cols - 1;
            let mut values: Vec<Value> = Vec::with_capacity(attr_cols);
            for i in 0..attr_cols {
                let v: Value = row
                    .get::<_, Value>(i)
                    .map_err(|e| Error::driver(self.driver_name, e))?;
                values.push(v);
            }
            // 末尾列は rowid。INTEGER PRIMARY KEY (rowid alias) は必ず i64 で取れる。
            let rowid: i64 = row
                .get::<_, i64>(attr_cols)
                .map_err(|e| Error::driver(self.driver_name, e))?;
            last_rowid_in_batch = rowid;
            out.push(values);
        }

        if out.is_empty() {
            self.done = true;
            return Ok(None);
        }
        // batch_size 未満で帰ってきたら以降は確実に空なので早期終了 (LIMIT は keyset の上限)。
        if out.len() < self.batch_size {
            self.done = true;
        }
        self.last_rowid = last_rowid_in_batch;
        Ok(Some(RowBatch { rows: out }))
    }
}

/// LIMIT/OFFSET 素朴 pagination iterator。
///
/// SQL テンプレートは `?1` に LIMIT、`?2` に OFFSET を bind する形式。SELECT の全列が
/// `RowBatch.rows` に入る (rowid 列の追加・削除は行わない)。
///
/// **注意**: 大きい OFFSET でスキャンが遅くなる。table が rowid を持つなら
/// [`KeysetRowsIter`] を優先すべき。
pub struct OffsetRowsIter<'a> {
    /// `KeysetRowsIter` と同じ理由で `&mut Connection` を取る (Send 要件)。
    conn: &'a mut Connection,
    sql: String,
    offset: i64,
    batch_size: usize,
    driver_name: &'static str,
    done: bool,
}

impl<'a> OffsetRowsIter<'a> {
    pub fn new(
        conn: &'a mut Connection,
        sql: String,
        batch_size: usize,
        driver_name: &'static str,
    ) -> Self {
        Self {
            conn,
            sql,
            offset: 0,
            batch_size,
            driver_name,
            done: false,
        }
    }

    pub fn next_batch(&mut self) -> Result<Option<RowBatch>> {
        if self.done {
            return Ok(None);
        }
        let limit_i64 = i64::try_from(self.batch_size).map_err(|_| {
            Error::driver_msg(
                self.driver_name,
                format!("batch_size {} exceeds i64::MAX", self.batch_size),
            )
        })?;

        let result = self.execute_batch(limit_i64);
        if result.is_err() {
            self.done = true;
        }
        result
    }

    fn execute_batch(&mut self, limit_i64: i64) -> Result<Option<RowBatch>> {
        let mut stmt = self
            .conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::driver(self.driver_name, e))?;
        let n_cols = stmt.column_count();

        let mut rows = stmt
            .query(params![limit_i64, self.offset])
            .map_err(|e| Error::driver(self.driver_name, e))?;

        let mut out: Vec<Vec<Value>> = Vec::with_capacity(self.batch_size);
        while let Some(row) = rows
            .next()
            .map_err(|e| Error::driver(self.driver_name, e))?
        {
            let mut values: Vec<Value> = Vec::with_capacity(n_cols);
            for i in 0..n_cols {
                let v: Value = row
                    .get::<_, Value>(i)
                    .map_err(|e| Error::driver(self.driver_name, e))?;
                values.push(v);
            }
            out.push(values);
        }

        if out.is_empty() {
            self.done = true;
            return Ok(None);
        }
        if out.len() < self.batch_size {
            self.done = true;
        }
        // i64 加算オーバーフローはテーブルサイズ的に起こり得ないが defensively saturate する。
        self.offset = self
            .offset
            .saturating_add(i64::try_from(out.len()).unwrap_or(i64::MAX));
        Ok(Some(RowBatch { rows: out }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    const DRIVER: &str = "test-driver";

    fn make_table_with_n_rows(n: i64) -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory open");
        // 行挿入は &Connection で実行し、KeysetRowsIter には &mut で渡す。
        conn.execute_batch(
            "CREATE TABLE t (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT,
                value INTEGER
            )",
        )
        .expect("create");
        for i in 1..=n {
            conn.execute(
                "INSERT INTO t (name, value) VALUES (?1, ?2)",
                params![format!("row-{i}"), i],
            )
            .expect("insert");
        }
        conn
    }

    #[test]
    fn keyset_iterates_100_rows_in_chunks_of_32() {
        let mut conn = make_table_with_n_rows(100);
        let sql =
            "SELECT name, value, rowid FROM t WHERE rowid > ?1 ORDER BY rowid LIMIT ?2".to_string();
        let mut it = KeysetRowsIter::new(&mut conn, sql, 32, DRIVER);

        let b1 = it.next_batch().unwrap().expect("batch 1");
        assert_eq!(b1.rows.len(), 32);
        let b2 = it.next_batch().unwrap().expect("batch 2");
        assert_eq!(b2.rows.len(), 32);
        let b3 = it.next_batch().unwrap().expect("batch 3");
        assert_eq!(b3.rows.len(), 32);
        let b4 = it.next_batch().unwrap().expect("batch 4");
        assert_eq!(b4.rows.len(), 4);
        assert!(it.next_batch().unwrap().is_none(), "exhausted");

        // 各 batch は (name, value) の 2 列を含む (rowid は内部消費)。
        assert_eq!(b1.rows[0].len(), 2);
        if let (Value::Text(name), Value::Integer(value)) = (&b1.rows[0][0], &b1.rows[0][1]) {
            assert_eq!(name, "row-1");
            assert_eq!(*value, 1);
        } else {
            panic!("unexpected types in row 0");
        }
    }

    #[test]
    fn keyset_empty_table_returns_none() {
        let mut conn = make_table_with_n_rows(0);
        let sql =
            "SELECT name, value, rowid FROM t WHERE rowid > ?1 ORDER BY rowid LIMIT ?2".to_string();
        let mut it = KeysetRowsIter::new(&mut conn, sql, 32, DRIVER);
        assert!(it.next_batch().unwrap().is_none());
    }

    #[test]
    fn keyset_with_where_clause_filters_rows() {
        let mut conn = make_table_with_n_rows(20);
        // user-supplied WHERE は rowid 条件と AND で結合する想定。
        let sql = "SELECT name, value, rowid FROM t WHERE (value > 15) AND rowid > ?1 ORDER BY rowid LIMIT ?2"
            .to_string();
        let mut it = KeysetRowsIter::new(&mut conn, sql, 32, DRIVER);
        let b = it.next_batch().unwrap().expect("batch");
        assert_eq!(b.rows.len(), 5); // 16, 17, 18, 19, 20
        assert!(it.next_batch().unwrap().is_none());
    }

    #[test]
    fn offset_iterates_100_rows_in_chunks_of_30() {
        let mut conn = make_table_with_n_rows(100);
        let sql = "SELECT name, value FROM t ORDER BY id LIMIT ?1 OFFSET ?2".to_string();
        let mut it = OffsetRowsIter::new(&mut conn, sql, 30, DRIVER);

        let b1 = it.next_batch().unwrap().expect("b1");
        assert_eq!(b1.rows.len(), 30);
        let b2 = it.next_batch().unwrap().expect("b2");
        assert_eq!(b2.rows.len(), 30);
        let b3 = it.next_batch().unwrap().expect("b3");
        assert_eq!(b3.rows.len(), 30);
        let b4 = it.next_batch().unwrap().expect("b4");
        assert_eq!(b4.rows.len(), 10);
        assert!(it.next_batch().unwrap().is_none());

        // OffsetRowsIter は SQL の全列を返す (rowid 内部消費なし)。
        assert_eq!(b1.rows[0].len(), 2);
    }

    #[test]
    fn offset_empty_returns_none() {
        let mut conn = make_table_with_n_rows(0);
        let sql = "SELECT name, value FROM t ORDER BY id LIMIT ?1 OFFSET ?2".to_string();
        let mut it = OffsetRowsIter::new(&mut conn, sql, 30, DRIVER);
        assert!(it.next_batch().unwrap().is_none());
    }

    #[test]
    fn keyset_full_batch_then_partial_then_none() {
        // batch_size = テーブル行数のとき、1 batch ぴったりで埋まり、次が None になる。
        let mut conn = make_table_with_n_rows(10);
        let sql =
            "SELECT name, value, rowid FROM t WHERE rowid > ?1 ORDER BY rowid LIMIT ?2".to_string();
        let mut it = KeysetRowsIter::new(&mut conn, sql, 10, DRIVER);
        let b = it.next_batch().unwrap().expect("b");
        assert_eq!(b.rows.len(), 10);
        // batch_size 一致でも次回は None (空 query)。done フラグは付かないが
        // 空 batch が走って自然に終わる。
        assert!(it.next_batch().unwrap().is_none());
    }
}
