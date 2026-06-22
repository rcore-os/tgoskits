// fwgorm: deterministic GORM language carpet.
//
// Covers gorm.io/gorm v1.31.1 + github.com/glebarez/sqlite v1.11.0 (pure-Go,
// CGO-free, modernc go-sqlite). Every assertion prints "ok: <label> = <value>"
// and is counted; the final line is GORM_COUNT=<n>.
//
// Determinism techniques:
//   - in-memory SQLite (:memory:) with SetMaxOpenConns(1) so the single shared
//     connection keeps the in-memory schema/data across calls;
//   - fixed NowFunc so CreatedAt/UpdatedAt are byte-exact;
//   - every test uses its own fresh DB (gorm_freshDB) so ordering is isolated;
//   - results asserted via round-trip read-back / sorted slices, never by raw
//     generated-SQL string equality where map/struct iteration order matters;
//   - no timestamps/addresses/random in output.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"sort"
	"strings"
	"time"

	"github.com/glebarez/sqlite"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
	"gorm.io/gorm/logger"
	"gorm.io/gorm/schema"
)

// gorm_discardLogger is an Info-level logger whose output is discarded; using it as
// the base logger keeps db.Debug() (which raises the level to Info and logs
// per-query timing) byte-deterministic -- the variable timing text goes nowhere.
var gorm_discardLogger = logger.New(
	log.New(io.Discard, "", 0),
	logger.Config{LogLevel: logger.Info},
)

// ---------------------------------------------------------------------------
// assertion harness
// ---------------------------------------------------------------------------

// gorm_fixedNow is the injected deterministic clock.
var gorm_fixedNow = time.Date(2020, 1, 2, 3, 4, 5, 0, time.UTC)

// gorm_freshDB returns a brand-new in-memory DB with a deterministic clock.
// SetMaxOpenConns(1) ensures the in-memory database survives across queries.
func gorm_freshDB() *gorm.DB {
	db, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{
		NowFunc: func() time.Time { return gorm_fixedNow },
		Logger:  gorm_discardLogger,
	})
	if err != nil {
		panic(err)
	}
	sqlDB, err := db.DB()
	if err != nil {
		panic(err)
	}
	sqlDB.SetMaxOpenConns(1)
	return db
}

// ---------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------

// User embeds gorm.Model (ID/CreatedAt/UpdatedAt/DeletedAt -> soft delete).
type User struct {
	gorm.Model
	Name  string `gorm:"column:name"`
	Email string `gorm:"uniqueIndex"`
	Age   int    `gorm:"default:18"`

	// associations
	Profile   Profile    // has-one
	Orders    []Order    // has-many
	Languages []Language `gorm:"many2many:user_languages"`

	// belongs-to Company
	CompanyID *uint
	Company   Company

	// transient computed field set by AfterFind; not persisted
	DisplayName string `gorm:"-"`
}

type Profile struct {
	ID     uint
	UserID uint
	Bio    string
}

type Order struct {
	ID     uint
	UserID uint
	State  string
	Amount int
}

type Language struct {
	ID   uint
	Name string
}

type Company struct {
	ID   uint
	Name string
}

// HardUser has NO gorm.Model / DeletedAt -> Delete is a physical (hard) delete.
type HardUser struct {
	ID   uint `gorm:"primaryKey"`
	Name string
	Age  int
}

// NotNullUser exercises the `not null` tag.
type NotNullUser struct {
	ID   uint   `gorm:"primaryKey"`
	Name string `gorm:"not null"`
}

// ConflictUser exercises clause.OnConflict upsert.
type ConflictUser struct {
	ID    uint   `gorm:"primaryKey"`
	Name  string `gorm:"uniqueIndex"`
	Count int
}

// ---------------------------------------------------------------------------
// hooks
// ---------------------------------------------------------------------------

type HookModel struct {
	ID   uint `gorm:"primaryKey"`
	Name string
	Age  int
}

var gorm_hookOrder []string

func (h *HookModel) BeforeSave(tx *gorm.DB) error   { gorm_hookOrder = append(gorm_hookOrder, "BeforeSave"); return nil }
func (h *HookModel) BeforeCreate(tx *gorm.DB) error { gorm_hookOrder = append(gorm_hookOrder, "BeforeCreate"); h.Name = "hooked-" + h.Name; return nil }
func (h *HookModel) AfterCreate(tx *gorm.DB) error  { gorm_hookOrder = append(gorm_hookOrder, "AfterCreate"); return nil }
func (h *HookModel) AfterSave(tx *gorm.DB) error    { gorm_hookOrder = append(gorm_hookOrder, "AfterSave"); return nil }
func (h *HookModel) AfterFind(tx *gorm.DB) error    { h.Name = h.Name + "!"; return nil }

func (h *HookModel) BeforeUpdate(tx *gorm.DB) error {
	if h.Age < 0 {
		return errors.New("age must be >= 0")
	}
	return nil
}

// GuardCreate errors on BeforeCreate -> exercises rollback.
type GuardCreate struct {
	ID   uint `gorm:"primaryKey"`
	Name string
}

func (g *GuardCreate) BeforeCreate(tx *gorm.DB) error { return errors.New("blocked") }

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

func runFrameworkGORM() {
	gorm_sectionOpenDialector()
	gorm_sectionMigratorSchema()
	gorm_sectionModelTags()
	gorm_sectionCreate()
	gorm_sectionQueryRead()
	gorm_sectionUpdateDelete()
	gorm_sectionQueryBuilding()
	gorm_sectionJoins()
	gorm_sectionAssociations()
	gorm_sectionTransactions()
	gorm_sectionHooks()
	gorm_sectionScopes()
	gorm_sectionRawSQL()
	gorm_sectionFirstOrCreate()
	gorm_sectionSessionsContext()
	gorm_sectionClauses()
	gorm_sectionGenerics()
}

// ---------------------------------------------------------------------------
// 1. Open & Dialector
// ---------------------------------------------------------------------------

func gorm_sectionOpenDialector() {
	fwOK("sqlite.DriverName", sqlite.DriverName)

	db, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{})
	fwMust("Open(:memory:) err", err)
	fwOK("Open(:memory:) db!=nil", db != nil)

	sqlDB, err := db.DB()
	fwMust("db.DB() err", err)
	fwMust("sqlDB.Ping()", sqlDB.Ping())

	db2, err := gorm.Open(sqlite.Open("file:fwgorm_carpet?mode=memory&cache=shared"), &gorm.Config{})
	fwMust("Open(file:...cache=shared) err", err)
	fwOK("Open(file:...cache=shared) db!=nil", db2 != nil)

	cdb := gorm_freshDB()
	fwMust("AutoMigrate(User) for NowFunc", cdb.AutoMigrate(&User{}))
	u := User{Name: "nowfunc", Email: "now@x"}
	fwMust("Create for NowFunc", cdb.Create(&u).Error)
	fwOK("NowFunc CreatedAt", u.CreatedAt.UTC().Format(time.RFC3339))
	fwOK("NowFunc UpdatedAt", u.UpdatedAt.UTC().Format(time.RFC3339))

	ddb := gorm_freshDB()
	fwMust("AutoMigrate(User) for DryRun", ddb.AutoMigrate(&User{}))
	dry := ddb.Session(&gorm.Session{DryRun: true})
	stmt := dry.Model(&User{}).Where("age > ?", 18).Find(&[]User{})
	fwOK("DryRun produced SQL non-empty", stmt.Statement.SQL.String() != "")
	var nDry int64
	ddb.Model(&User{}).Count(&nDry)
	fwOK("DryRun did not write (count)", nDry)

	pdb, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{PrepareStmt: true})
	fwMust("Open(PrepareStmt) err", err)
	fwMust("AutoMigrate(PrepareStmt)", pdb.AutoMigrate(&HardUser{}))
	fwMust("Create(PrepareStmt)", pdb.Create(&HardUser{ID: 1, Name: "p"}).Error)
	var pn int64
	pdb.Model(&HardUser{}).Count(&pn)
	fwOK("PrepareStmt count", pn)

	sdb, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{SkipDefaultTransaction: true})
	fwMust("Open(SkipDefaultTransaction) err", err)
	fwMust("AutoMigrate(SkipDefaultTransaction)", sdb.AutoMigrate(&HardUser{}))
	fwMust("Create(SkipDefaultTransaction)", sdb.Create(&HardUser{ID: 1, Name: "s"}).Error)

	bdb, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{CreateBatchSize: 2})
	fwMust("Open(CreateBatchSize) err", err)
	fwMust("AutoMigrate(CreateBatchSize)", bdb.AutoMigrate(&HardUser{}))
	bus := []HardUser{{ID: 1, Name: "a"}, {ID: 2, Name: "b"}, {ID: 3, Name: "c"}}
	tx := bdb.Create(&bus)
	fwOK("CreateBatchSize RowsAffected", tx.RowsAffected)

	adb, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{AllowGlobalUpdate: true})
	fwMust("Open(AllowGlobalUpdate) err", err)
	fwMust("AutoMigrate(AllowGlobalUpdate)", adb.AutoMigrate(&HardUser{}))
	adb.Create(&HardUser{ID: 1, Name: "g", Age: 1})
	gtx := adb.Model(&HardUser{}).Update("age", 99)
	fwMust("AllowGlobalUpdate no WHERE err", gtx.Error)

	ndb, err := gorm.Open(sqlite.Open(":memory:"), &gorm.Config{
		NamingStrategy: schema.NamingStrategy{TablePrefix: "t_"},
	})
	fwMust("Open(NamingStrategy) err", err)
	fwMust("AutoMigrate(NamingStrategy)", ndb.AutoMigrate(&HardUser{}))
	fwOK("NamingStrategy prefixed table exists", ndb.Migrator().HasTable("t_hard_users"))

	edb := gorm_freshDB()
	edb.AddError(errors.New("sentinel"))
	fwOK("AddError surfaces", edb.Error != nil)

	npd := gorm_freshDB()
	psd, err := npd.DB()
	fwMust("NewPreparedStmtDB underlying pool err", err)
	pdbw := gorm.NewPreparedStmtDB(psd, 100, time.Hour)
	fwOK("NewPreparedStmtDB non-nil", pdbw != nil)
}

// ---------------------------------------------------------------------------
// 2. Migrator / Schema
// ---------------------------------------------------------------------------

func gorm_sectionMigratorSchema() {
	db := gorm_freshDB()
	m := db.Migrator()

	fwMust("Migrator.CreateTable(User)", m.CreateTable(&User{}))
	fwOK("HasTable(User) after create", m.HasTable(&User{}))

	tables, err := m.GetTables()
	fwMust("GetTables err", err)
	sort.Strings(tables)
	fwOK("GetTables gorm_contains users", gorm_contains(tables, "users"))

	fwOK("HasColumn(User,Name)", m.HasColumn(&User{}, "Name"))
	fwOK("HasColumn(User,Nope)", m.HasColumn(&User{}, "Nope"))

	fwMust("AutoMigrate(HardUser) for cols", db.AutoMigrate(&HardUser{}))
	fwOK("HasColumn(HardUser,Age)", m.HasColumn(&HardUser{}, "Age"))

	cts, err := m.ColumnTypes(&User{})
	fwMust("ColumnTypes err", err)
	var idType string
	var names []string
	for _, ct := range cts {
		names = append(names, ct.Name())
		if ct.Name() == "id" {
			idType = strings.ToUpper(ct.DatabaseTypeName())
		}
	}
	sort.Strings(names)
	fwOK("ColumnTypes has created_at", gorm_contains(names, "created_at"))
	fwOK("ColumnTypes id type integer-ish", strings.Contains(idType, "INT"))

	idxName := "idx_users_email"
	fwOK("HasIndex(User, idx_users_email)", m.HasIndex(&User{}, idxName))
	fwOK("HasIndex by field Email", m.HasIndex(&User{}, "Email"))

	fwMust("DropTable(HardUser)", m.DropTable(&HardUser{}))
	fwOK("HasTable(HardUser) after drop", m.HasTable(&HardUser{}))
}

// ---------------------------------------------------------------------------
// 3. Model tags & conventions
// ---------------------------------------------------------------------------

func gorm_sectionModelTags() {
	db := gorm_freshDB()
	fwMust("AutoMigrate(User) tags", db.AutoMigrate(&User{}))
	u1 := User{Name: "a", Email: "a@x"}
	u2 := User{Name: "b", Email: "b@x"}
	db.Create(&u1)
	db.Create(&u2)
	fwOK("autoIncrement first ID", u1.ID)
	fwOK("autoIncrement second ID", u2.ID)

	fwOK("gorm.Model CreatedAt set", u1.CreatedAt.Equal(gorm_fixedNow))

	dup := User{Name: "c", Email: "a@x"}
	err := db.Create(&dup).Error
	fwOK("uniqueIndex duplicate rejected", err != nil)

	nndb := gorm_freshDB()
	fwMust("AutoMigrate(NotNullUser)", nndb.AutoMigrate(&NotNullUser{}))
	nnErr := nndb.Exec("INSERT INTO not_null_users (id, name) VALUES (1, NULL)").Error
	fwOK("not null NULL insert rejected", nnErr != nil)

	ddb := gorm_freshDB()
	fwMust("AutoMigrate(User) default", ddb.AutoMigrate(&User{}))
	fwMust("Create map omit age", ddb.Model(&User{}).Create(map[string]interface{}{"name": "d", "email": "d@x"}).Error)
	var du User
	ddb.Where("email = ?", "d@x").First(&du)
	fwOK("default:18 applied", du.Age)

	m := ddb.Migrator()
	cts, _ := m.ColumnTypes(&User{})
	hasCreatedAt := false
	for _, ct := range cts {
		if ct.Name() == "created_at" {
			hasCreatedAt = true
		}
	}
	fwOK("NamingStrategy table users", m.HasTable("users"))
	fwOK("NamingStrategy column created_at", hasCreatedAt)
	fwOK("column tag name", m.HasColumn(&User{}, "name"))
}

// ---------------------------------------------------------------------------
// 4. Create
// ---------------------------------------------------------------------------

func gorm_sectionCreate() {
	db := gorm_freshDB()
	fwMust("AutoMigrate(User) create", db.AutoMigrate(&User{}))

	u := User{Name: "a", Email: "a@x"}
	tx := db.Create(&u)
	fwMust("Create err", tx.Error)
	fwOK("Create RowsAffected", tx.RowsAffected)
	fwOK("Create ID populated", u.ID)

	db2 := gorm_freshDB()
	db2.AutoMigrate(&User{})
	us := []User{{Name: "p", Email: "p@x"}, {Name: "q", Email: "q@x"}, {Name: "r", Email: "r@x"}}
	tx2 := db2.Create(&us)
	fwOK("Create slice RowsAffected", tx2.RowsAffected)
	var n2 int64
	db2.Model(&User{}).Count(&n2)
	fwOK("Create slice count", n2)
	fwOK("Create slice first ID", us[0].ID)
	fwOK("Create slice third ID", us[2].ID)

	db3 := gorm_freshDB()
	db3.AutoMigrate(&User{})
	var bus []User
	for i := 0; i < 5; i++ {
		bus = append(bus, User{Name: fmt.Sprintf("b%d", i), Email: fmt.Sprintf("b%d@x", i)})
	}
	tx3 := db3.CreateInBatches(&bus, 2)
	fwOK("CreateInBatches RowsAffected", tx3.RowsAffected)
	var n3 int64
	db3.Model(&User{}).Count(&n3)
	fwOK("CreateInBatches count", n3)

	db4 := gorm_freshDB()
	db4.AutoMigrate(&User{})
	su := User{Name: "save", Email: "s@x", Age: 5}
	db4.Save(&su)
	fwOK("Save insert ID>0", su.ID > 0)
	savedID := su.ID
	su.Age = 50
	db4.Save(&su)
	var reloaded User
	db4.First(&reloaded, savedID)
	fwOK("Save update age", reloaded.Age)
	fwOK("Save same ID", reloaded.ID == savedID)

	db5 := gorm_freshDB()
	db5.AutoMigrate(&User{})
	ou := User{Name: "omit", Email: "o@x", Age: 77}
	db5.Omit("Age").Create(&ou)
	var or User
	db5.First(&or, ou.ID)
	fwOK("Omit age (default 18)", or.Age)

	db6 := gorm_freshDB()
	db6.AutoMigrate(&User{})
	selU := User{Name: "sel", Email: "sel@x", Age: 33}
	db6.Select("Name", "Email").Create(&selU)
	var sr User
	db6.First(&sr, selU.ID)
	fwOK("Create Select restricts (age default)", sr.Age)
}

// ---------------------------------------------------------------------------
// 5. Query / Read
// ---------------------------------------------------------------------------

func gorm_seedUsers(db *gorm.DB, specs ...[2]interface{}) {
	for i, s := range specs {
		db.Create(&User{Name: s[0].(string), Email: fmt.Sprintf("seed%d@x", i), Age: s[1].(int)})
	}
}

func gorm_sectionQueryRead() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	gorm_seedUsers(db, [2]interface{}{"x", 20}, [2]interface{}{"y", 30}, [2]interface{}{"z", 40})

	var u User
	fwMust("First err", db.First(&u).Error)
	fwOK("First lowest PK", u.ID)
	var u2 User
	db.First(&u2, 2)
	fwOK("First by PK cond", u2.ID)

	var ul User
	db.Last(&ul)
	fwOK("Last highest PK", ul.ID)

	var ut User
	fwMust("Take err", db.Take(&ut).Error)
	fwOK("Take returns a row", ut.ID > 0)

	edb := gorm_freshDB()
	edb.AutoMigrate(&User{})
	var ue User
	err := edb.First(&ue).Error
	fwOK("ErrRecordNotFound", errors.Is(err, gorm.ErrRecordNotFound))

	var us []User
	tx := db.Find(&us)
	fwOK("Find len", len(us))
	fwOK("Find RowsAffected", tx.RowsAffected)

	var us2 []User
	db.Find(&us2, []int{1, 2})
	fwOK("Find by PK slice len", len(us2))

	var us3 []User
	tx3 := edb.Find(&us3)
	fwOK("Find empty len", len(us3))
	fwOK("Find empty no error", tx3.Error == nil)

	var ages []int
	db.Model(&User{}).Order("age").Pluck("age", &ages)
	fwOK("Pluck ages", fmt.Sprint(ages))

	var out struct{ Name string }
	db.Model(&User{}).Select("name").Where("id = ?", 1).Scan(&out)
	fwOK("Scan name", out.Name)

	var tk User
	db.Where(&User{Age: 30}).Take(&tk)
	fwOK("Take struct cond age", tk.Age)
}

// ---------------------------------------------------------------------------
// 6. Update / Delete
// ---------------------------------------------------------------------------

func gorm_sectionUpdateDelete() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	db.Create(&User{Name: "u", Email: "u@x", Age: 20})

	tx := db.Model(&User{}).Where("id = ?", 1).Update("age", 99)
	fwOK("Update RowsAffected", tx.RowsAffected)
	var r User
	db.First(&r, 1)
	fwOK("Update age applied", r.Age)

	db2 := gorm_freshDB()
	db2.AutoMigrate(&User{})
	db2.Create(&User{Name: "orig", Email: "o2@x", Age: 7})
	var r2 User
	db2.First(&r2, 1)
	db2.Model(&r2).Updates(User{Name: "newname", Age: 0})
	var rr2 User
	db2.First(&rr2, 1)
	fwOK("Updates struct name changed", rr2.Name)
	fwOK("Updates struct zero-age skipped", rr2.Age)

	db2.Model(&User{}).Where("id = ?", 1).Updates(map[string]interface{}{"age": 0})
	var rr3 User
	db2.First(&rr3, 1)
	fwOK("Updates map zero written", rr3.Age)

	db3 := gorm_freshDB()
	db3.AutoMigrate(&User{})
	db3.Create(&User{Name: "c", Email: "c@x", Age: 1})
	var before User
	db3.First(&before, 1)
	db3.Model(&User{}).Where("id = ?", 1).UpdateColumn("age", 5)
	var after User
	db3.First(&after, 1)
	fwOK("UpdateColumn age", after.Age)
	fwOK("UpdateColumn UpdatedAt unchanged", after.UpdatedAt.Equal(before.UpdatedAt))

	db3.Model(&User{}).Where("id = ?", 1).UpdateColumns(map[string]interface{}{"age": 8, "name": "cc"})
	var after2 User
	db3.First(&after2, 1)
	fwOK("UpdateColumns age", after2.Age)
	fwOK("UpdateColumns name", after2.Name)

	gdb := gorm_freshDB()
	gdb.AutoMigrate(&User{})
	gdb.Create(&User{Name: "g", Email: "g@x", Age: 1})
	gErr := gdb.Model(&User{}).Update("age", 1).Error
	fwOK("ErrMissingWhereClause", errors.Is(gErr, gorm.ErrMissingWhereClause))

	gErr2 := gdb.Session(&gorm.Session{AllowGlobalUpdate: true}).Model(&User{}).Update("age", 2).Error
	fwOK("AllowGlobalUpdate session ok", gErr2 == nil)

	hdb := gorm_freshDB()
	hdb.AutoMigrate(&HardUser{})
	hdb.Create(&HardUser{ID: 1, Name: "h1"})
	hdb.Create(&HardUser{ID: 2, Name: "h2"})
	htx := hdb.Delete(&HardUser{ID: 1})
	fwOK("Hard delete RowsAffected", htx.RowsAffected)
	var hn int64
	hdb.Model(&HardUser{}).Count(&hn)
	fwOK("Hard delete remaining", hn)

	sdb := gorm_freshDB()
	sdb.AutoMigrate(&User{})
	gorm_seedUsers(sdb, [2]interface{}{"a", 1}, [2]interface{}{"b", 2}, [2]interface{}{"c", 3})
	sdb.Delete(&User{}, 1)
	var sn int64
	sdb.Model(&User{}).Count(&sn)
	fwOK("Soft delete default count (hidden)", sn)
	var un int64
	sdb.Unscoped().Model(&User{}).Count(&un)
	fwOK("Soft delete Unscoped count (visible)", un)
	var deleted User
	sdb.Unscoped().First(&deleted, 1)
	fwOK("Soft delete DeletedAt set", deleted.DeletedAt.Valid)

	bdb := gorm_freshDB()
	bdb.AutoMigrate(&HardUser{})
	bdb.Create(&[]HardUser{{ID: 1, Name: "x"}, {ID: 2, Name: "y"}, {ID: 3, Name: "z"}})
	btx := bdb.Delete(&HardUser{}, []int{1, 2})
	fwOK("Batch delete RowsAffected", btx.RowsAffected)
	var bn int64
	bdb.Model(&HardUser{}).Count(&bn)
	fwOK("Batch delete remaining", bn)

	udb := gorm_freshDB()
	udb.AutoMigrate(&User{})
	gorm_seedUsers(udb, [2]interface{}{"a", 1}, [2]interface{}{"b", 2})
	udb.Delete(&User{}, 1)
	udb.Unscoped().Where("id = ?", 1).Delete(&User{})
	var uc int64
	udb.Unscoped().Model(&User{}).Count(&uc)
	fwOK("Unscoped hard delete remaining", uc)
}

// ---------------------------------------------------------------------------
// 7. Query building
// ---------------------------------------------------------------------------

func gorm_sectionQueryBuilding() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	gorm_seedUsers(db, [2]interface{}{"a", 20}, [2]interface{}{"a", 30}, [2]interface{}{"b", 40})

	var us []User
	db.Where("age > ?", 25).Find(&us)
	fwOK("Where string", len(us))

	var um []User
	db.Where(map[string]interface{}{"age": 20}).Find(&um)
	fwOK("Where map", len(um))

	var ust []User
	db.Where(&User{Age: 20}).Find(&ust)
	fwOK("Where struct", len(ust))

	var uo []User
	db.Where("age = ?", 20).Or("age = ?", 40).Find(&uo)
	fwOK("Or union", len(uo))

	var un []User
	db.Not("age = ?", 30).Find(&un)
	fwOK("Not excludes", len(un))

	var uord []User
	db.Order("age desc").Find(&uord)
	fwOK("Order desc first", uord[0].Age)
	fwOK("Order desc last", uord[len(uord)-1].Age)

	ldb := gorm_freshDB()
	ldb.AutoMigrate(&User{})
	for i := 0; i < 5; i++ {
		ldb.Create(&User{Name: fmt.Sprintf("l%d", i), Email: fmt.Sprintf("l%d@x", i), Age: i})
	}
	var ul []User
	ldb.Order("id").Limit(2).Offset(1).Find(&ul)
	fwOK("Limit/Offset len", len(ul))
	fwOK("Limit/Offset first ID", ul[0].ID)

	var sel []User
	db.Select("name").Where("id = ?", 1).Find(&sel)
	fwOK("Select name populated", sel[0].Name)
	fwOK("Select age zero", sel[0].Age)

	var names []string
	db.Model(&User{}).Distinct("name").Order("name").Pluck("name", &names)
	fwOK("Distinct names", fmt.Sprint(names))

	var grows []struct {
		Name string
		C    int
	}
	db.Model(&User{}).Select("name, count(*) c").Group("name").Having("count(*) > ?", 1).Scan(&grows)
	fwOK("Group+Having rows", len(grows))
	if len(grows) > 0 {
		fwOK("Group+Having name", grows[0].Name)
		fwOK("Group+Having count", grows[0].C)
	}

	var cn int64
	db.Model(&User{}).Where("age > ?", 0).Count(&cn)
	fwOK("Count age>0", cn)

	var tn int64
	db.Table("users").Count(&tn)
	fwOK("Table raw count", tn)

	var mc []struct {
		Renamed string
	}
	db.Model(&User{}).Select("name").Where("id = ?", 1).
		MapColumns(map[string]string{"name": "renamed"}).Scan(&mc)
	if len(mc) > 0 {
		fwOK("MapColumns remap", mc[0].Renamed)
	} else {
		fwOK("MapColumns remap", "(no row)")
	}
}

// ---------------------------------------------------------------------------
// 8. Joins
// ---------------------------------------------------------------------------

func gorm_sectionJoins() {
	db := gorm_freshDB()
	db.AutoMigrate(&Company{}, &User{})

	c := Company{Name: "Acme"}
	db.Create(&c)
	cid := c.ID
	db.Create(&User{Name: "withco", Email: "wc@x", Age: 1, CompanyID: &cid})
	db.Create(&User{Name: "noco", Email: "nc@x", Age: 2})

	var u User
	db.Joins("Company").Where("users.name = ?", "withco").First(&u)
	fwOK("Joins relation populated", u.Company.Name)

	var us []User
	db.Joins("JOIN companies on companies.id = users.company_id").
		Where("companies.name = ?", "Acme").Find(&us)
	fwOK("Joins raw filtered len", len(us))

	var ius []User
	db.InnerJoins("Company").Find(&ius)
	fwOK("InnerJoins len (only matched)", len(ius))
}

// UserLanguage is an explicit many2many join table for SetupJoinTable.
type UserLanguage struct {
	UserID     uint `gorm:"primaryKey"`
	LanguageID uint `gorm:"primaryKey"`
}

// ---------------------------------------------------------------------------
// 9. Associations
// ---------------------------------------------------------------------------

func gorm_sectionAssociations() {
	db := gorm_freshDB()
	fwMust("AutoMigrate(assoc)", db.AutoMigrate(&Company{}, &Profile{}, &Order{}, &Language{}, &User{}))

	u := User{
		Name:    "assoc",
		Email:   "assoc@x",
		Age:     1,
		Profile: Profile{Bio: "hello"},
		Orders: []Order{
			{State: "paid", Amount: 10},
			{State: "cancelled", Amount: 20},
		},
		Languages: []Language{{Name: "Go"}, {Name: "Rust"}},
	}
	fwMust("Create nested assoc", db.Create(&u).Error)

	var pu User
	db.Preload("Profile").First(&pu, u.ID)
	fwOK("Preload Profile ID>0", pu.Profile.ID > 0)
	fwOK("Preload Profile bio", pu.Profile.Bio)

	var hm User
	db.Preload("Orders").First(&hm, u.ID)
	fwOK("Preload Orders len", len(hm.Orders))

	var pc User
	db.Preload("Orders", "state = ?", "paid").First(&pc, u.ID)
	fwOK("Preload Orders filtered len", len(pc.Orders))

	var m2 User
	db.Preload("Languages").First(&m2, u.ID)
	fwOK("Preload Languages len", len(m2.Languages))
	var jcount int64
	db.Table("user_languages").Count(&jcount)
	fwOK("join table rows", jcount)

	bdb := gorm_freshDB()
	bdb.AutoMigrate(&User{}, &Order{})
	bu := User{Name: "owner", Email: "owner@x", Age: 1}
	bdb.Create(&bu)
	o := Order{UserID: bu.ID, State: "x", Amount: 1}
	bdb.Create(&o)
	var orow Order
	bdb.First(&orow, o.ID)
	fwOK("BelongsTo UserID matches", orow.UserID == bu.ID)

	cnt := db.Model(&u).Association("Orders").Count()
	fwOK("Association Count initial", cnt)
	fwMust("Association Append", db.Model(&u).Association("Orders").Append(&Order{State: "new", Amount: 5}))
	fwOK("Association Count after append", db.Model(&u).Association("Orders").Count())

	var found []Order
	fwMust("Association Find", db.Model(&u).Association("Orders").Find(&found))
	fwOK("Association Find len", len(found))

	fwMust("Association Replace", db.Model(&u).Association("Languages").Replace(&Language{Name: "Zig"}))
	fwOK("Association Languages after replace", db.Model(&u).Association("Languages").Count())

	var lang Language
	db.Where("name = ?", "Zig").First(&lang)
	fwMust("Association Delete", db.Model(&u).Association("Languages").Delete(&lang))
	fwOK("Association Languages after delete", db.Model(&u).Association("Languages").Count())

	fwMust("Association Clear", db.Model(&u).Association("Orders").Clear())
	fwOK("Association Count after clear", db.Model(&u).Association("Orders").Count())

	jdb := gorm_freshDB()
	jdb.AutoMigrate(&Language{})
	fwMust("SetupJoinTable", jdb.SetupJoinTable(&User{}, "Languages", &UserLanguage{}))
	fwMust("AutoMigrate after SetupJoinTable", jdb.AutoMigrate(&User{}))
	fwOK("SetupJoinTable join exists", jdb.Migrator().HasTable("user_languages"))
}

// ---------------------------------------------------------------------------
// 10. Transactions
// ---------------------------------------------------------------------------

func gorm_sectionTransactions() {
	db := gorm_freshDB()
	db.AutoMigrate(&HardUser{})
	errTx := db.Transaction(func(tx *gorm.DB) error {
		tx.Create(&HardUser{ID: 1, Name: "rollme"})
		return errors.New("boom")
	})
	fwOK("Transaction returns error", errTx != nil)
	var n int64
	db.Model(&HardUser{}).Count(&n)
	fwOK("Transaction rolled back count", n)

	okTx := db.Transaction(func(tx *gorm.DB) error {
		tx.Create(&HardUser{ID: 2, Name: "keep"})
		return nil
	})
	fwMust("Transaction commit err", okTx)
	db.Model(&HardUser{}).Count(&n)
	fwOK("Transaction committed count", n)

	mdb := gorm_freshDB()
	mdb.AutoMigrate(&HardUser{})
	tx := mdb.Begin()
	tx.Create(&HardUser{ID: 1, Name: "r"})
	tx.Rollback()
	var rn int64
	mdb.Model(&HardUser{}).Count(&rn)
	fwOK("Begin+Rollback count", rn)

	tx2 := mdb.Begin()
	tx2.Create(&HardUser{ID: 2, Name: "c"})
	tx2.Commit()
	mdb.Model(&HardUser{}).Count(&rn)
	fwOK("Begin+Commit count", rn)

	sdb := gorm_freshDB()
	sdb.AutoMigrate(&HardUser{})
	stx := sdb.Begin()
	stx.Create(&HardUser{ID: 1, Name: "u1"})
	stx.SavePoint("sp1")
	stx.Create(&HardUser{ID: 2, Name: "u2"})
	stx.RollbackTo("sp1")
	stx.Commit()
	var sn int64
	sdb.Model(&HardUser{}).Count(&sn)
	fwOK("SavePoint/RollbackTo count", sn)
	var keptName string
	sdb.Model(&HardUser{}).Where("id = ?", 1).Pluck("name", &keptName)
	fwOK("SavePoint kept u1", keptName)

	ndb := gorm_freshDB()
	ndb.AutoMigrate(&HardUser{})
	nerr := ndb.Transaction(func(tx *gorm.DB) error {
		tx.Create(&HardUser{ID: 1, Name: "outer"})
		_ = tx.Transaction(func(tx2 *gorm.DB) error {
			tx2.Create(&HardUser{ID: 2, Name: "inner"})
			return errors.New("inner fail")
		})
		return nil
	})
	fwMust("Nested outer err", nerr)
	var nn int64
	ndb.Model(&HardUser{}).Count(&nn)
	fwOK("Nested kept outer only count", nn)
	var oname string
	ndb.Model(&HardUser{}).Where("id = ?", 1).Pluck("name", &oname)
	fwOK("Nested outer row kept", oname)
}

// ---------------------------------------------------------------------------
// 11. Hooks
// ---------------------------------------------------------------------------

func gorm_sectionHooks() {
	db := gorm_freshDB()
	db.AutoMigrate(&HookModel{})

	gorm_hookOrder = nil
	h := HookModel{Name: "alice"}
	db.Create(&h)
	fwOK("BeforeCreate mutated name", h.Name)

	var hf HookModel
	db.First(&hf, h.ID)
	fwOK("AfterFind appended !", hf.Name)

	fwOK("Hook call order", strings.Join(gorm_hookOrder, ","))

	db2 := gorm_freshDB()
	db2.AutoMigrate(&HookModel{})
	hu := HookModel{Name: "x", Age: 5}
	db2.Create(&hu)
	hu.Age = -1
	uerr := db2.Model(&hu).Updates(map[string]interface{}{"age": -1}).Error
	fwOK("BeforeUpdate rejects negative", uerr != nil)
	var hr HookModel
	db2.First(&hr, hu.ID)
	fwOK("BeforeUpdate row unchanged age", hr.Age)

	gdb := gorm_freshDB()
	gdb.AutoMigrate(&GuardCreate{})
	gerr := gdb.Create(&GuardCreate{ID: 1, Name: "blocked"}).Error
	fwOK("Hook error returned", gerr != nil)
	var gn int64
	gdb.Model(&GuardCreate{}).Count(&gn)
	fwOK("Hook error rollback count", gn)

	serr := gdb.Session(&gorm.Session{SkipHooks: true}).Create(&GuardCreate{ID: 2, Name: "ok"}).Error
	fwMust("SkipHooks create err", serr)
	gdb.Model(&GuardCreate{}).Count(&gn)
	fwOK("SkipHooks created count", gn)
}

// ---------------------------------------------------------------------------
// 12. Scopes
// ---------------------------------------------------------------------------

func gorm_ageGT(n int) func(*gorm.DB) *gorm.DB {
	return func(d *gorm.DB) *gorm.DB { return d.Where("age > ?", n) }
}
func gorm_ageLT(n int) func(*gorm.DB) *gorm.DB {
	return func(d *gorm.DB) *gorm.DB { return d.Where("age < ?", n) }
}

func gorm_sectionScopes() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	gorm_seedUsers(db, [2]interface{}{"a", 20}, [2]interface{}{"b", 30}, [2]interface{}{"c", 40})

	var us []User
	db.Scopes(gorm_ageGT(25)).Find(&us)
	fwOK("Scope single len", len(us))

	var us2 []User
	db.Scopes(gorm_ageGT(15), gorm_ageLT(35)).Find(&us2)
	fwOK("Scopes chained len", len(us2))
}

// ---------------------------------------------------------------------------
// 13. Raw SQL
// ---------------------------------------------------------------------------

func gorm_sectionRawSQL() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	gorm_seedUsers(db, [2]interface{}{"a", 20}, [2]interface{}{"b", 30}, [2]interface{}{"c", 40})

	var n int64
	db.Raw("SELECT count(*) FROM users WHERE age > ?", 0).Scan(&n)
	fwOK("Raw scalar count", n)

	var u User
	db.Raw("SELECT * FROM users WHERE id = ?", 1).Scan(&u)
	fwOK("Raw row name", u.Name)

	tx := db.Exec("UPDATE users SET age = age + ? WHERE id = ?", 1, 1)
	fwOK("Exec RowsAffected", tx.RowsAffected)
	var r User
	db.First(&r, 1)
	fwOK("Exec incremented age", r.Age)

	old := r.Age
	db.Model(&User{}).Where("id = ?", 1).Update("age", gorm.Expr("age + ?", 5))
	var r2 User
	db.First(&r2, 1)
	fwOK("gorm.Expr atomic add", r2.Age == old+5)

	rows, err := db.Model(&User{}).Order("id").Rows()
	fwMust("Rows err", err)
	iter := 0
	for rows.Next() {
		var ru User
		db.ScanRows(rows, &ru)
		iter++
	}
	rows.Close()
	fwOK("Rows/ScanRows iterations", iter)

	row := db.Raw("SELECT count(*) FROM users").Row()
	var rc int
	row.Scan(&rc)
	fwOK("Row scalar", rc)

	sql := db.ToSQL(func(tx *gorm.DB) *gorm.DB {
		return tx.Model(&User{}).Where("age > ?", 18).Find(&[]User{})
	})
	fwOK("ToSQL gorm_contains WHERE", strings.Contains(sql, "WHERE"))
	fwOK("ToSQL inlines arg 18", strings.Contains(sql, "18"))
}

// ---------------------------------------------------------------------------
// 14. FirstOrCreate / FirstOrInit / Assign / Attrs
// ---------------------------------------------------------------------------

func gorm_sectionFirstOrCreate() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	var u User
	tx := db.Where(User{Name: "new", Email: "new@x"}).FirstOrCreate(&u)
	fwOK("FirstOrCreate insert RowsAffected", tx.RowsAffected)
	var n int64
	db.Model(&User{}).Count(&n)
	fwOK("FirstOrCreate count", n)
	var u2 User
	tx2 := db.Where(User{Name: "new", Email: "new@x"}).FirstOrCreate(&u2)
	fwOK("FirstOrCreate found RowsAffected", tx2.RowsAffected)

	adb := gorm_freshDB()
	adb.AutoMigrate(&User{})
	var ua User
	adb.Where(User{Name: "x", Email: "ax@x"}).Attrs(User{Age: 20}).FirstOrCreate(&ua)
	fwOK("Attrs not-found Age", ua.Age)
	adb.Create(&User{Name: "x2", Email: "ax2@x", Age: 5})
	var ua2 User
	adb.Where(User{Name: "x2"}).Attrs(User{Age: 20}).FirstOrCreate(&ua2)
	fwOK("Attrs found keeps Age", ua2.Age)

	gdb := gorm_freshDB()
	gdb.AutoMigrate(&User{})
	gdb.Create(&User{Name: "g", Email: "ag@x", Age: 5})
	var ug User
	gdb.Where(User{Name: "g"}).Assign(User{Age: 9}).FirstOrCreate(&ug)
	var gr User
	gdb.Where("name = ?", "g").First(&gr)
	fwOK("Assign updates found Age", gr.Age)

	idb := gorm_freshDB()
	idb.AutoMigrate(&User{})
	var ui User
	idb.Where(User{Name: "i", Email: "ai@x"}).Attrs(User{Age: 7}).FirstOrInit(&ui)
	fwOK("FirstOrInit name", ui.Name)
	fwOK("FirstOrInit attr age", ui.Age)
	var in int64
	idb.Model(&User{}).Count(&in)
	fwOK("FirstOrInit no write count", in)
}

// ---------------------------------------------------------------------------
// 15. Sessions / Context / Misc
// ---------------------------------------------------------------------------

func gorm_sectionSessionsContext() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	gorm_seedUsers(db, [2]interface{}{"a", 20}, [2]interface{}{"b", 30})

	dry := db.Session(&gorm.Session{DryRun: true})
	st := dry.Model(&User{}).Where("age > ?", 0).Find(&[]User{})
	fwOK("Session DryRun SQL non-empty", st.Statement.SQL.String() != "")

	scoped := db.Where("age > ?", 100)
	var resetUS []User
	scoped.Session(&gorm.Session{NewDB: true}).Find(&resetUS)
	fwOK("Session NewDB resets conds len", len(resetUS))

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	var cu []User
	cerr := db.WithContext(ctx).Find(&cu).Error
	fwOK("WithContext canceled", errors.Is(cerr, context.Canceled))

	var du User
	derr := db.Debug().First(&du).Error
	fwOK("Debug query succeeds", derr == nil)

	sqlDB, err := db.DB()
	fwMust("db.DB() pool err", err)
	fwMust("pool Ping", sqlDB.Ping())

	rtx := db.Model(&User{}).Where("age > ?", 0).Update("age", 99)
	fwMust("chained tx.Error", rtx.Error)
	fwOK("chained tx.RowsAffected", rtx.RowsAffected)

	sdb := db.Set("my:key", "val")
	v, found := sdb.Get("my:key")
	fwOK("Set/Get found", found)
	fwOK("Set/Get value", v)

	ldb := db.Session(&gorm.Session{Logger: logger.Default.LogMode(logger.Error)})
	var ln int64
	ldb.Model(&User{}).Count(&ln)
	fwOK("Session Logger count", ln)
}

// ---------------------------------------------------------------------------
// 16. clause package
// ---------------------------------------------------------------------------

func gorm_sectionClauses() {
	db := gorm_freshDB()
	db.AutoMigrate(&ConflictUser{})
	db.Create(&ConflictUser{ID: 1, Name: "dup", Count: 1})
	db.Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "name"}},
		DoUpdates: clause.Assignments(map[string]interface{}{"count": 99}),
	}).Create(&ConflictUser{ID: 2, Name: "dup", Count: 5})
	var cu ConflictUser
	db.Where("name = ?", "dup").First(&cu)
	fwOK("OnConflict DoUpdates count", cu.Count)
	var ccount int64
	db.Model(&ConflictUser{}).Count(&ccount)
	fwOK("OnConflict no duplicate row count", ccount)

	ndb := gorm_freshDB()
	ndb.AutoMigrate(&ConflictUser{})
	ndb.Create(&ConflictUser{ID: 1, Name: "x", Count: 1})
	ndb.Clauses(clause.OnConflict{DoNothing: true}).Create(&ConflictUser{ID: 1, Name: "x", Count: 9})
	var xu ConflictUser
	ndb.First(&xu, 1)
	fwOK("OnConflict DoNothing keeps original count", xu.Count)

	rdb := gorm_freshDB()
	rdb.AutoMigrate(&User{})
	rdb.Create(&User{Name: "ret", Email: "ret@x", Age: 5})
	var ret []User
	rdb.Model(&User{}).Clauses(clause.Returning{Columns: []clause.Column{{Name: "age"}}}).
		Where("name = ?", "ret").Update("age", 42)
	rdb.Where("name = ?", "ret").Find(&ret)
	fwOK("Returning update age", ret[0].Age)

	ldb := gorm_freshDB()
	ldb.AutoMigrate(&User{})
	ldb.Create(&User{Name: "lock", Email: "lock@x", Age: 1})
	// glebarez/sqlite strips row-level locking (SQLite has none): clause.Locking
	// is accepted, the query still runs, and FOR UPDATE is correctly absent.
	lsql := ldb.ToSQL(func(tx *gorm.DB) *gorm.DB {
		return tx.Clauses(clause.Locking{Strength: "UPDATE"}).Model(&User{}).Find(&[]User{})
	})
	fwOK("Locking accepted, FOR UPDATE stripped by sqlite", !strings.Contains(strings.ToUpper(lsql), "FOR UPDATE"))
	var lockUS []User
	lerr := ldb.Clauses(clause.Locking{Strength: "UPDATE"}).Find(&lockUS).Error
	fwOK("Locking query runs (no error)", lerr == nil)
	fwOK("Locking query rows", len(lockUS))

	edb := gorm_freshDB()
	edb.AutoMigrate(&User{})
	gorm_seedUsers(edb, [2]interface{}{"a", 20}, [2]interface{}{"b", 30})
	var es []User
	edb.Clauses(clause.Expr{SQL: "age > ?", Vars: []interface{}{25}}).Find(&es)
	fwOK("clause.Expr where len", len(es))

	var ne []User
	edb.Where(clause.NamedExpr{SQL: "age > @min", Vars: []interface{}{
		map[string]interface{}{"min": 25},
	}}).Find(&ne)
	fwOK("clause.NamedExpr where len", len(ne))

	cdb := gorm_freshDB()
	cdb.AutoMigrate(&User{})
	gorm_seedUsers(cdb, [2]interface{}{"alpha", 20}, [2]interface{}{"beta", 30}, [2]interface{}{"gamma", 40})

	var inUS []User
	cdb.Clauses(clause.Where{Exprs: []clause.Expression{
		clause.IN{Column: "age", Values: []interface{}{20, 40}},
	}}).Find(&inUS)
	fwOK("clause.IN len", len(inUS))

	var eqUS []User
	cdb.Clauses(clause.Eq{Column: "age", Value: 30}).Find(&eqUS)
	fwOK("clause.Eq len", len(eqUS))

	var gtUS []User
	cdb.Clauses(clause.Gt{Column: "age", Value: 25}).Find(&gtUS)
	fwOK("clause.Gt len", len(gtUS))

	var gteUS []User
	cdb.Clauses(clause.Gte{Column: "age", Value: 30}).Find(&gteUS)
	fwOK("clause.Gte len", len(gteUS))

	var ltUS []User
	cdb.Clauses(clause.Lt{Column: "age", Value: 30}).Find(&ltUS)
	fwOK("clause.Lt len", len(ltUS))

	var lteUS []User
	cdb.Clauses(clause.Lte{Column: "age", Value: 30}).Find(&lteUS)
	fwOK("clause.Lte len", len(lteUS))

	var neqUS []User
	cdb.Clauses(clause.Neq{Column: "age", Value: 30}).Find(&neqUS)
	fwOK("clause.Neq len", len(neqUS))

	var likeUS []User
	cdb.Clauses(clause.Like{Column: "name", Value: "a%"}).Find(&likeUS)
	fwOK("clause.Like len", len(likeUS))

	var obUS []User
	cdb.Clauses(clause.OrderBy{Columns: []clause.OrderByColumn{
		{Column: clause.Column{Name: "age"}, Desc: true},
	}}).Find(&obUS)
	fwOK("clause.OrderBy desc first age", obUS[0].Age)
}

// ---------------------------------------------------------------------------
// 17. Generics API (v1.30+ / v1.31.1: gorm.G[T], gorm.WithResult)
//     Type-safe, context-first API. All operations take context.Context.
// ---------------------------------------------------------------------------

func gorm_sectionGenerics() {
	db := gorm_freshDB()
	db.AutoMigrate(&User{})
	ctx := context.Background()

	// gorm.G[T](db).Create(ctx, &t)
	gu := User{Name: "gen1", Email: "g1@x", Age: 21}
	fwMust("Generics Create", gorm.G[User](db).Create(ctx, &gu))
	fwOK("Generics Create assigned ID", gu.ID > 0)

	// gorm.WithResult() captures RowsAffected on a generic Create
	res := gorm.WithResult()
	fwMust("Generics Create WithResult", gorm.G[User](db, res).Create(ctx, &User{Name: "gen2", Email: "g2@x", Age: 22}))
	fwOK("Generics WithResult RowsAffected", res.RowsAffected)

	// gorm.G[T](db).Where(...).First(ctx) -> (T, error)
	got, err := gorm.G[User](db).Where("name = ?", "gen1").First(ctx)
	fwMust("Generics First err", err)
	fwOK("Generics First name", got.Name)
	fwOK("Generics First age", got.Age)

	// Take / Last
	tk, terr := gorm.G[User](db).Where("age = ?", 22).Take(ctx)
	fwMust("Generics Take err", terr)
	fwOK("Generics Take name", tk.Name)

	last, lerr := gorm.G[User](db).Last(ctx)
	fwMust("Generics Last err", lerr)
	fwOK("Generics Last name", last.Name)

	// Find(ctx) -> ([]T, error)
	all, ferr := gorm.G[User](db).Find(ctx)
	fwMust("Generics Find err", ferr)
	fwOK("Generics Find len", len(all))

	// Where + Order + Limit chained, then Find
	chained, cerr := gorm.G[User](db).Where("age > ?", 0).Order("age desc").Limit(1).Find(ctx)
	fwMust("Generics chained Find err", cerr)
	fwOK("Generics chained first age", chained[0].Age)

	// Count(ctx, column)
	n, nerr := gorm.G[User](db).Where("age > ?", 0).Count(ctx, "*")
	fwMust("Generics Count err", nerr)
	fwOK("Generics Count", n)

	// Update(ctx, name, value) -> rowsAffected
	ra, uerr := gorm.G[User](db).Where("name = ?", "gen1").Update(ctx, "age", 99)
	fwMust("Generics Update err", uerr)
	fwOK("Generics Update rowsAffected", ra)
	after, _ := gorm.G[User](db).Where("name = ?", "gen1").First(ctx)
	fwOK("Generics Update applied age", after.Age)

	// Updates(ctx, T)
	rua, uuerr := gorm.G[User](db).Where("name = ?", "gen1").Updates(ctx, User{Age: 5})
	fwMust("Generics Updates err", uuerr)
	fwOK("Generics Updates rowsAffected", rua)

	// ErrRecordNotFound surfaces from generic First
	_, gerr := gorm.G[User](db).Where("name = ?", "nope").First(ctx)
	fwOK("Generics First ErrRecordNotFound", errors.Is(gerr, gorm.ErrRecordNotFound))

	// Delete(ctx) -> rowsAffected (soft delete on gorm.Model)
	dra, derr := gorm.G[User](db).Where("name = ?", "gen2").Delete(ctx)
	fwMust("Generics Delete err", derr)
	fwOK("Generics Delete rowsAffected", dra)

	// Raw + Scan via generics
	var scalar int64
	fwMust("Generics Raw Scan", gorm.G[User](db).Raw("SELECT count(*) FROM users WHERE deleted_at IS NULL").Scan(ctx, &scalar))
	fwOK("Generics Raw count", scalar)

	// Exec (DML) via generics
	fwMust("Generics Exec", gorm.G[User](db).Exec(ctx, "UPDATE users SET age = age + 1 WHERE name = ?", "gen1"))
	incd, _ := gorm.G[User](db).Where("name = ?", "gen1").First(ctx)
	fwOK("Generics Exec incremented age", incd.Age)

	// CreateInBatches via generics
	batch := []User{
		{Name: "gb1", Email: "gb1@x", Age: 1},
		{Name: "gb2", Email: "gb2@x", Age: 2},
		{Name: "gb3", Email: "gb3@x", Age: 3},
	}
	fwMust("Generics CreateInBatches", gorm.G[User](db).CreateInBatches(ctx, &batch, 2))
	bn, _ := gorm.G[User](db).Where("name LIKE ?", "gb%").Count(ctx, "*")
	fwOK("Generics CreateInBatches count", bn)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

func gorm_contains(ss []string, s string) bool {
	for _, x := range ss {
		if x == s {
			return true
		}
	}
	return false
}
