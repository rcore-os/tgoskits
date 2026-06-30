package main

import (
	"bytes"
	"fmt"
	"reflect"
	"strings"
	"text/template"

	htmltemplate "html/template"
)

// ---------------------------------------------------------------------------
// text/template + html/template.
// ---------------------------------------------------------------------------

func runStdTemplates() {
	section("std-templates")

	// text/template: field access, range, if, pipelines.
	tpl := template.Must(template.New("t").Parse(
		`Hi {{.Name}}, you have {{len .Items}} items: {{range $i, $v := .Items}}{{if $i}}, {{end}}{{$v}}{{end}}.`))
	var buf bytes.Buffer
	_ = tpl.Execute(&buf, struct {
		Name  string
		Items []string
	}{Name: "leo", Items: []string{"a", "b", "c"}})
	chkStr("text/template/render", buf.String(), "Hi leo, you have 3 items: a, b, c.")

	// Custom function map + pipeline.
	fn := template.FuncMap{"up": strings.ToUpper}
	tpl2 := template.Must(template.New("t2").Funcs(fn).Parse(`{{.x | up}}`))
	var b2 bytes.Buffer
	_ = tpl2.Execute(&b2, map[string]string{"x": "go"})
	chkStr("text/template/funcmap", b2.String(), "GO")

	// with / else.
	tpl3 := template.Must(template.New("t3").Parse(`{{if .Ok}}yes{{else}}no{{end}}`))
	var b3 bytes.Buffer
	_ = tpl3.Execute(&b3, struct{ Ok bool }{Ok: false})
	chkStr("text/template/if-else", b3.String(), "no")

	// html/template auto-escapes (context-aware).
	htpl := htmltemplate.Must(htmltemplate.New("h").Parse(`<p>{{.}}</p>`))
	var hb bytes.Buffer
	_ = htpl.Execute(&hb, "<script>alert(1)</script>")
	chkStr("html/template/escape", hb.String(), "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>")

	// html/template attribute-context escaping.
	atpl := htmltemplate.Must(htmltemplate.New("a").Parse(`<a href="{{.}}">x</a>`))
	var ab bytes.Buffer
	_ = atpl.Execute(&ab, "a&b")
	chkStr("html/template/attr-escape", ab.String(), `<a href="a&amp;b">x</a>`)
}

// ---------------------------------------------------------------------------
// reflect — including struct tags, kinds, and dynamic value inspection.
// ---------------------------------------------------------------------------

type tagged struct {
	Name string `json:"name" validate:"required"`
	Age  int    `json:"age"`
	skip bool   // unexported
}

func runStdReflect() {
	section("std-reflect")

	// TypeOf / Kind / Name.
	chkStr("reflect/TypeOf-int", reflect.TypeOf(42).Kind().String(), "int")
	chkStr("reflect/TypeOf-string", reflect.TypeOf("x").Kind().String(), "string")
	chkStr("reflect/TypeOf-slice", reflect.TypeOf([]int{}).Kind().String(), "slice")
	chkStr("reflect/TypeOf-map", reflect.TypeOf(map[string]int{}).Kind().String(), "map")
	chkStr("reflect/TypeOf-ptr", reflect.TypeOf(&tagged{}).Kind().String(), "ptr")
	chkStr("reflect/TypeName", reflect.TypeOf(tagged{}).Name(), "tagged")

	// ValueOf + Int/String/Bool.
	chk("reflect/ValueOf-Int", int(reflect.ValueOf(int64(99)).Int()), 99)
	chkStr("reflect/ValueOf-String", reflect.ValueOf("go").String(), "go")
	chkTrue("reflect/ValueOf-Bool", reflect.ValueOf(true).Bool())

	// Struct field enumeration + tags.
	t := reflect.TypeOf(tagged{})
	chk("reflect/NumField", t.NumField(), 3)
	f0 := t.Field(0)
	chkStr("reflect/Field-name", f0.Name, "Name")
	chkStr("reflect/Field-tag-json", f0.Tag.Get("json"), "name")
	chkStr("reflect/Field-tag-validate", f0.Tag.Get("validate"), "required")
	chk("reflect/Field-exported", boolToInt(f0.IsExported()), 1)
	chk("reflect/Field-unexported", boolToInt(t.Field(2).IsExported()), 0)

	// Read field values dynamically.
	v := reflect.ValueOf(tagged{Name: "leo", Age: 30})
	chkStr("reflect/value-field-name", v.Field(0).String(), "leo")
	chk("reflect/value-field-age", int(v.Field(1).Int()), 30)

	// Set a field via a pointer (Elem + addressable).
	obj := &tagged{}
	pv := reflect.ValueOf(obj).Elem()
	pv.FieldByName("Name").SetString("set!")
	pv.FieldByName("Age").SetInt(7)
	chkStr("reflect/Set-string", obj.Name, "set!")
	chk("reflect/Set-int", obj.Age, 7)

	// DeepEqual.
	chkTrue("reflect/DeepEqual-slice", reflect.DeepEqual([]int{1, 2}, []int{1, 2}))
	chk("reflect/DeepEqual-false", boolToInt(reflect.DeepEqual([]int{1}, []int{2})), 0)
	chkTrue("reflect/DeepEqual-map", reflect.DeepEqual(
		map[string]int{"a": 1}, map[string]int{"a": 1}))

	// Slice element inspection.
	sv := reflect.ValueOf([]string{"x", "y", "z"})
	chk("reflect/slice-len", sv.Len(), 3)
	chkStr("reflect/slice-index", sv.Index(1).String(), "y")
	chkStr("reflect/slice-elem-type", sv.Type().Elem().Kind().String(), "string")

	// Map inspection: sorted keys for determinism.
	mv := reflect.ValueOf(map[string]int{"b": 2, "a": 1})
	chk("reflect/map-len", mv.Len(), 2)
	var keys []string
	for _, k := range mv.MapKeys() {
		keys = append(keys, k.String())
	}
	// sort for stable output
	if len(keys) == 2 && keys[0] > keys[1] {
		keys[0], keys[1] = keys[1], keys[0]
	}
	chkStr("reflect/map-keys-sorted", strings.Join(keys, ","), "a,b")

	// Func reflection: NumIn / NumOut.
	ft := reflect.TypeOf(func(a int, b string) (bool, error) { return false, nil })
	chk("reflect/Func-NumIn", ft.NumIn(), 2)
	chk("reflect/Func-NumOut", ft.NumOut(), 2)
	chkStr("reflect/Func-In0", ft.In(0).Kind().String(), "int")

	// reflect.New + Call a function dynamically.
	add := func(a, b int) int { return a + b }
	res := reflect.ValueOf(add).Call([]reflect.Value{
		reflect.ValueOf(3), reflect.ValueOf(4),
	})
	chk("reflect/Call", int(res[0].Int()), 7)

	// Zero value + IsZero.
	chkTrue("reflect/IsZero", reflect.ValueOf(tagged{}).IsZero())
	chk("reflect/IsZero-false", boolToInt(reflect.ValueOf(tagged{Age: 1}).IsZero()), 0)

	// Sanity: %v of a typed value via reflect.
	chkStr("reflect/Interface", fmt.Sprint(reflect.ValueOf(5).Interface()), "5")
}
