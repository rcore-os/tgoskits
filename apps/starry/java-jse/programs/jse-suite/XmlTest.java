import javax.xml.XMLConstants;
import javax.xml.parsers.*;
import javax.xml.xpath.*;
import javax.xml.namespace.*;
import javax.xml.transform.*;
import javax.xml.transform.dom.*;
import javax.xml.transform.stream.*;
import javax.xml.transform.sax.*;
import javax.xml.stream.*;
import javax.xml.stream.events.*;
import javax.xml.validation.*;
import javax.xml.datatype.*;
import org.w3c.dom.*;
import org.xml.sax.*;
import org.xml.sax.helpers.*;
import org.xml.sax.ext.*;
import java.io.*;
import java.util.*;

/*
 * Carpet-level coverage of the JDK 17 java.xml module:
 *   - org.w3c.dom + javax.xml.parsers  (DOM parse, build, mutate, namespaces, CharacterData, Attr, NamedNodeMap, DOMImplementation)
 *   - javax.xml.xpath                  (compile/evaluate, all XPathConstants types, axes, functions, NamespaceContext, XPathVariableResolver)
 *   - org.xml.sax + helpers + ext      (SAXParser, DefaultHandler/DefaultHandler2, Attributes, XMLReader, LexicalHandler, error handling)
 *   - javax.xml.stream                 (StAX cursor reader, event reader, stream writer)
 *   - javax.xml.transform              (identity serialization, DOMSource/StreamSource/SAXSource/DOMResult, OutputKeys, XSLT)
 *   - javax.xml.validation             (XSD SchemaFactory/Schema/Validator, valid + invalid)
 *   - javax.xml.datatype               (DatatypeFactory, XMLGregorianCalendar, Duration, DatatypeConstants)
 *   - javax.xml.namespace.QName + javax.xml.XMLConstants
 * All inputs are self-contained string literals; no network, no external files. Assertions are exact.
 */
public class XmlTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String n) { if (c) ok++; else { fail++; System.out.println("FAIL " + n); } }
    static void eq(Object a, Object b, String n) { check(a == null ? b == null : a.equals(b), n); }
    static void eqi(long a, long b, String n) { if (a == b) ok++; else { fail++; System.out.println("FAIL " + n + " got=" + a + " want=" + b); } }

    interface Run { void run() throws Throwable; }
    static void expect(Class<? extends Throwable> ex, Run r, String n) {
        try { r.run(); fail++; System.out.println("FAIL " + n + " (no throw)"); }
        catch (Throwable t) { check(ex.isInstance(t), n); }
    }

    static final ErrorHandler RETHROW = new ErrorHandler() {
        public void warning(SAXParseException e) {}
        public void error(SAXParseException e) throws SAXException { throw e; }
        public void fatalError(SAXParseException e) throws SAXException { throw e; }
    };

    static DocumentBuilder builder(boolean ns) throws Exception {
        DocumentBuilderFactory f = DocumentBuilderFactory.newInstance();
        f.setNamespaceAware(ns);
        f.setFeature(XMLConstants.FEATURE_SECURE_PROCESSING, true);
        DocumentBuilder b = f.newDocumentBuilder();
        b.setErrorHandler(RETHROW);
        return b;
    }

    static Document parse(String xml, boolean ns) throws Exception {
        return builder(ns).parse(new InputSource(new StringReader(xml)));
    }

    public static void main(String[] args) {
        run(XmlTest::domBasics, "domBasics");
        run(XmlTest::domNodeTypes, "domNodeTypes");
        run(XmlTest::domBuildMutate, "domBuildMutate");
        run(XmlTest::domCharacterData, "domCharacterData");
        run(XmlTest::domNamedNodeMap, "domNamedNodeMap");
        run(XmlTest::domNamespaces, "domNamespaces");
        run(XmlTest::domImplAndId, "domImplAndId");
        run(XmlTest::domImportClone, "domImportClone");
        run(XmlTest::xpathTypes, "xpathTypes");
        run(XmlTest::xpathFunctionsAxes, "xpathFunctionsAxes");
        run(XmlTest::xpathNamespaceVariable, "xpathNamespaceVariable");
        run(XmlTest::saxParsing, "saxParsing");
        run(XmlTest::saxLexical, "saxLexical");
        run(XmlTest::staxCursor, "staxCursor");
        run(XmlTest::staxEvents, "staxEvents");
        run(XmlTest::staxWriter, "staxWriter");
        run(XmlTest::transformIdentity, "transformIdentity");
        run(XmlTest::transformSources, "transformSources");
        run(XmlTest::transformXslt, "transformXslt");
        run(XmlTest::validation, "validation");
        run(XmlTest::datatype, "datatype");
        run(XmlTest::qnameAndConstants, "qnameAndConstants");
        run(XmlTest::errorPaths, "errorPaths");

        System.out.println("XML_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("XML_DONE");
    }

    static void run(Run r, String section) {
        try { r.run(); }
        catch (Throwable t) { fail++; System.out.println("FAIL section-" + section + ": " + t); }
    }

    // ---------------------------------------------------------------- DOM basics
    static void domBasics() throws Throwable {
        String xml = "<root attr=\"top\"><item id=\"1\">a</item><item id=\"2\">b</item><item id=\"3\">c</item></root>";
        Document doc = parse(xml, false);
        Element root = doc.getDocumentElement();
        eq(root.getNodeName(), "root", "dom-root-name");
        eq(root.getTagName(), "root", "dom-root-tagname");
        eqi(root.getNodeType(), Node.ELEMENT_NODE, "dom-root-type");
        eq(doc.getNodeName(), "#document", "dom-doc-name");
        eqi(doc.getNodeType(), Node.DOCUMENT_NODE, "dom-doc-type");
        check(doc.getDoctype() == null, "dom-no-doctype");

        NodeList items = doc.getElementsByTagName("item");
        eqi(items.getLength(), 3, "dom-items-count");
        eq(items.item(0).getTextContent(), "a", "dom-text0");
        eq(items.item(2).getTextContent(), "c", "dom-text2");
        check(items.item(3) == null, "dom-item-oob-null");

        Element it1 = (Element) items.item(0);
        eq(it1.getAttribute("id"), "1", "dom-attr-id");
        check(it1.hasAttribute("id"), "dom-hasattr");
        check(!it1.hasAttribute("missing"), "dom-hasattr-no");
        eq(it1.getAttribute("missing"), "", "dom-attr-missing-empty");
        Attr a = it1.getAttributeNode("id");
        eq(a.getName(), "id", "dom-attrnode-name");
        eq(a.getValue(), "1", "dom-attrnode-value");
        check(a.getOwnerElement() == it1, "dom-attr-owner");
        check(a.getSpecified(), "dom-attr-specified");

        // traversal
        check(root.hasChildNodes(), "dom-haschildren");
        eqi(root.getChildNodes().getLength(), 3, "dom-childnodes");
        check(root.getFirstChild() == items.item(0), "dom-firstchild");
        check(root.getLastChild() == items.item(2), "dom-lastchild");
        check(items.item(0).getNextSibling() == items.item(1), "dom-nextsibling");
        check(items.item(1).getPreviousSibling() == items.item(0), "dom-prevsibling");
        check(items.item(0).getParentNode() == root, "dom-parent");
        check(it1.getOwnerDocument() == doc, "dom-ownerdoc");
        eq(root.getAttribute("attr"), "top", "dom-root-attr");

        // getElementsByTagName wildcard
        eqi(doc.getElementsByTagName("*").getLength(), 4, "dom-wildcard");

        // entity reference expansion via internal DTD subset (offline)
        Document ent = parse("<!DOCTYPE r [<!ENTITY e \"XY\">]><r>&e;Z</r>", false);
        eq(ent.getDocumentElement().getTextContent(), "XYZ", "dom-entity-expand");
        eqi(ent.getDoctype() != null ? 1 : 0, 1, "dom-has-doctype");
        eq(ent.getDoctype().getName(), "r", "dom-doctype-name");
    }

    // ---------------------------------------------------------------- node types
    static void domNodeTypes() throws Throwable {
        Document doc = parse("<r><![CDATA[<x>&]]><!--cmt--><?pi dat?>txt</r>", false);
        Element r = doc.getDocumentElement();
        NodeList kids = r.getChildNodes();
        eqi(kids.getLength(), 4, "type-kids");
        eqi(kids.item(0).getNodeType(), Node.CDATA_SECTION_NODE, "type-cdata");
        eq(((CDATASection) kids.item(0)).getData(), "<x>&", "type-cdata-data");
        eqi(kids.item(1).getNodeType(), Node.COMMENT_NODE, "type-comment");
        eq(((org.w3c.dom.Comment) kids.item(1)).getData(), "cmt", "type-comment-data");
        eqi(kids.item(2).getNodeType(), Node.PROCESSING_INSTRUCTION_NODE, "type-pi");
        org.w3c.dom.ProcessingInstruction pi = (org.w3c.dom.ProcessingInstruction) kids.item(2);
        eq(pi.getTarget(), "pi", "type-pi-target");
        eq(pi.getData(), "dat", "type-pi-data");
        eqi(kids.item(3).getNodeType(), Node.TEXT_NODE, "type-text");
        eq(((Text) kids.item(3)).getData(), "txt", "type-text-data");
        // getTextContent concatenates CDATA + text (comment/PI excluded)
        eq(r.getTextContent(), "<x>&txt", "type-textcontent");
    }

    // ---------------------------------------------------------------- build + mutate
    static void domBuildMutate() throws Throwable {
        DocumentBuilder b = builder(false);
        Document doc = b.newDocument();
        Element root = doc.createElement("root");
        doc.appendChild(root);
        check(doc.getDocumentElement() == root, "build-docelem");

        Element c1 = doc.createElement("c");
        c1.setAttribute("k", "v");
        Text t = doc.createTextNode("hello");
        c1.appendChild(t);
        root.appendChild(c1);
        eqi(root.getChildNodes().getLength(), 1, "build-append");
        eq(c1.getAttribute("k"), "v", "build-setattr");
        eq(c1.getTextContent(), "hello", "build-text");

        // insertBefore
        Element c0 = doc.createElement("c0");
        root.insertBefore(c0, c1);
        check(root.getFirstChild() == c0, "build-insertbefore");
        eqi(root.getChildNodes().getLength(), 2, "build-insert-count");

        // replaceChild
        Element rep = doc.createElement("rep");
        Node old = root.replaceChild(rep, c0);
        check(old == c0, "build-replace-return");
        check(root.getFirstChild() == rep, "build-replace-now");

        // removeChild
        Node removed = root.removeChild(rep);
        check(removed == rep, "build-remove-return");
        eqi(root.getChildNodes().getLength(), 1, "build-remove-count");

        // attribute removal + setAttributeNode
        c1.removeAttribute("k");
        check(!c1.hasAttribute("k"), "build-removeattr");
        Attr at = doc.createAttribute("z");
        at.setValue("9");
        c1.setAttributeNode(at);
        eq(c1.getAttribute("z"), "9", "build-setattrnode");

        // setNodeValue on text
        t.setNodeValue("world");
        eq(c1.getTextContent(), "world", "build-setnodevalue");

        // normalize merges adjacent text
        c1.appendChild(doc.createTextNode("!!"));
        eqi(countTextNodes(c1), 2, "build-pre-normalize");
        c1.normalize();
        eqi(countTextNodes(c1), 1, "build-post-normalize");
        eq(c1.getTextContent(), "world!!", "build-normalize-merge");

        // wrong-document insertion
        Document other = b.newDocument();
        Element foreign = other.createElement("f");
        expect(DOMException.class, () -> root.appendChild(foreign), "build-wrongdoc-throws");

        // HIERARCHY: appending an element as child of itself
        expect(DOMException.class, () -> c1.appendChild(c1), "build-cycle-throws");
    }

    static int countTextNodes(Node n) {
        int c = 0;
        NodeList l = n.getChildNodes();
        for (int i = 0; i < l.getLength(); i++) if (l.item(i).getNodeType() == Node.TEXT_NODE) c++;
        return c;
    }

    // ---------------------------------------------------------------- CharacterData
    static void domCharacterData() throws Throwable {
        Document doc = builder(false).newDocument();
        Text t = doc.createTextNode("Hello World");
        eqi(t.getLength(), 11, "cd-length");
        eq(t.substringData(0, 5), "Hello", "cd-substring");
        eq(t.substringData(6, 5), "World", "cd-substring2");
        t.appendData("!");
        eq(t.getData(), "Hello World!", "cd-append");
        t.insertData(5, ",");
        eq(t.getData(), "Hello, World!", "cd-insert");
        t.deleteData(5, 1);
        eq(t.getData(), "Hello World!", "cd-delete");
        t.replaceData(0, 5, "Howdy");
        eq(t.getData(), "Howdy World!", "cd-replace");
        t.setData("ABCDEF");
        // splitText
        Text right = t.splitText(3);
        eq(t.getData(), "ABC", "cd-split-left");
        eq(right.getData(), "DEF", "cd-split-right");

        org.w3c.dom.Comment cm = doc.createComment("note");
        eq(cm.getData(), "note", "cd-comment-data");
        eqi(cm.getLength(), 4, "cd-comment-length");

        CDATASection cds = doc.createCDATASection("<raw>");
        eq(cds.getData(), "<raw>", "cd-cdata-data");

        // substring out of range -> INDEX_SIZE_ERR
        Text t2 = doc.createTextNode("abc");
        expect(DOMException.class, () -> t2.substringData(10, 1), "cd-substring-oob");
    }

    // ---------------------------------------------------------------- NamedNodeMap
    static void domNamedNodeMap() throws Throwable {
        Document doc = parse("<e a=\"1\" b=\"2\" c=\"3\"/>", false);
        Element e = doc.getDocumentElement();
        NamedNodeMap m = e.getAttributes();
        eqi(m.getLength(), 3, "nnm-length");
        eq(m.getNamedItem("b").getNodeValue(), "2", "nnm-getnamed");
        check(m.getNamedItem("zzz") == null, "nnm-missing-null");
        // item() ordering is impl-defined; just verify all present
        Set<String> names = new HashSet<>();
        for (int i = 0; i < m.getLength(); i++) names.add(m.item(i).getNodeName());
        check(names.equals(new HashSet<>(Arrays.asList("a", "b", "c"))), "nnm-items");

        Attr na = doc.createAttribute("d");
        na.setValue("4");
        m.setNamedItem(na);
        eqi(m.getLength(), 4, "nnm-setnamed");
        eq(e.getAttribute("d"), "4", "nnm-setnamed-effect");
        Node rem = m.removeNamedItem("a");
        eq(rem.getNodeName(), "a", "nnm-removed-name");
        eqi(m.getLength(), 3, "nnm-after-remove");
        expect(DOMException.class, () -> m.removeNamedItem("nope"), "nnm-remove-missing-throws");
    }

    // ---------------------------------------------------------------- namespaces
    static void domNamespaces() throws Throwable {
        String xml = "<root xmlns:p=\"urn:p\" xmlns=\"urn:def\" p:a=\"1\"><p:child>v</p:child><kid>w</kid></root>";
        Document doc = parse(xml, true);
        Element root = doc.getDocumentElement();
        eq(root.getNamespaceURI(), "urn:def", "ns-root-uri");
        eq(root.getLocalName(), "root", "ns-root-local");
        check(root.getPrefix() == null, "ns-root-prefix-null");

        eq(root.getAttributeNS("urn:p", "a"), "1", "ns-attr-getns");
        check(root.hasAttributeNS("urn:p", "a"), "ns-hasattrns");

        NodeList pchild = doc.getElementsByTagNameNS("urn:p", "child");
        eqi(pchild.getLength(), 1, "ns-getbytagns");
        Element child = (Element) pchild.item(0);
        eq(child.getNamespaceURI(), "urn:p", "ns-child-uri");
        eq(child.getLocalName(), "child", "ns-child-local");
        eq(child.getPrefix(), "p", "ns-child-prefix");

        // default namespace element
        Element kid = (Element) doc.getElementsByTagNameNS("urn:def", "kid").item(0);
        eq(kid.getNamespaceURI(), "urn:def", "ns-kid-uri");

        // wildcard NS
        eqi(doc.getElementsByTagNameNS("*", "child").getLength(), 1, "ns-wildcard-uri");
        eqi(doc.getElementsByTagNameNS("urn:p", "*").getLength(), 1, "ns-wildcard-local");

        // lookups
        eq(root.lookupNamespaceURI("p"), "urn:p", "ns-lookup-uri");
        eq(root.lookupPrefix("urn:p"), "p", "ns-lookup-prefix");
        check(root.isDefaultNamespace("urn:def"), "ns-isdefault");

        // programmatic createElementNS / setAttributeNS
        Document d2 = builder(true).newDocument();
        Element en = d2.createElementNS("urn:q", "q:e");
        eq(en.getPrefix(), "q", "ns-create-prefix");
        eq(en.getLocalName(), "e", "ns-create-local");
        en.setAttributeNS("urn:q", "q:k", "5");
        eq(en.getAttributeNS("urn:q", "k"), "5", "ns-create-attrns");
    }

    // ---------------------------------------------------------------- DOMImplementation + getElementById
    static void domImplAndId() throws Throwable {
        DocumentBuilder b = builder(true);
        DOMImplementation impl = b.getDOMImplementation();
        check(impl.hasFeature("XML", "3.0"), "impl-hasfeature-xml");
        check(impl.hasFeature("Core", null), "impl-hasfeature-core");

        DocumentType dt = impl.createDocumentType("html", "-//pub", "sys.dtd");
        eq(dt.getName(), "html", "impl-doctype-name");
        eq(dt.getPublicId(), "-//pub", "impl-doctype-public");
        eq(dt.getSystemId(), "sys.dtd", "impl-doctype-system");

        Document nd = impl.createDocument("urn:root", "r:doc", null);
        eq(nd.getDocumentElement().getNamespaceURI(), "urn:root", "impl-createdoc-ns");
        eq(nd.getDocumentElement().getNodeName(), "r:doc", "impl-createdoc-name");

        // getElementById via setIdAttribute
        Document doc = b.newDocument();
        Element root = doc.createElement("root");
        doc.appendChild(root);
        Element x = doc.createElement("x");
        x.setAttribute("id", "k1");
        x.setIdAttribute("id", true);
        root.appendChild(x);
        check(doc.getElementById("k1") == x, "impl-getbyid");
        check(doc.getElementById("nope") == null, "impl-getbyid-miss");
    }

    // ---------------------------------------------------------------- importNode + cloneNode + adoptNode
    static void domImportClone() throws Throwable {
        DocumentBuilder b = builder(false);
        Document src = parse("<a x=\"1\"><b>deep</b></a>", false);
        Element a = src.getDocumentElement();

        // shallow clone (no children)
        Element shallow = (Element) a.cloneNode(false);
        eq(shallow.getNodeName(), "a", "clone-shallow-name");
        eq(shallow.getAttribute("x"), "1", "clone-shallow-attr");
        check(!shallow.hasChildNodes(), "clone-shallow-nochild");
        check(shallow != a, "clone-shallow-distinct");

        // deep clone
        Element deep = (Element) a.cloneNode(true);
        check(deep.hasChildNodes(), "clone-deep-child");
        eq(deep.getTextContent(), "deep", "clone-deep-text");

        // importNode into another document
        Document dst = b.newDocument();
        Element imp = (Element) dst.importNode(a, true);
        check(imp.getOwnerDocument() == dst, "import-owner");
        eq(imp.getTextContent(), "deep", "import-text");
        check(a.getOwnerDocument() == src, "import-src-unchanged");

        // adoptNode moves ownership
        Document src2 = parse("<m><n/></m>", false);
        Element m = src2.getDocumentElement();
        Node adopted = dst.adoptNode(m);
        check(adopted.getOwnerDocument() == dst, "adopt-owner");
    }

    // ---------------------------------------------------------------- XPath return types
    static void xpathTypes() throws Throwable {
        Document doc = library();
        XPath xp = XPathFactory.newInstance().newXPath();

        // STRING (default)
        eq(xp.evaluate("/library/book[2]/title", doc), "Beta", "xp-string-default");
        eq(xp.evaluate("/library/book[@id='b3']/title/text()", doc), "Gamma", "xp-string-predicate");

        // NUMBER
        Double n = (Double) xp.evaluate("count(//book)", doc, XPathConstants.NUMBER);
        eqi(n.intValue(), 3, "xp-number-count");
        Double sum = (Double) xp.evaluate("sum(//price)", doc, XPathConstants.NUMBER);
        eqi(sum.intValue(), 60, "xp-number-sum");

        // BOOLEAN
        Boolean t = (Boolean) xp.evaluate("//book[@cat='fiction']", doc, XPathConstants.BOOLEAN);
        check(t, "xp-boolean-true");
        Boolean f = (Boolean) xp.evaluate("//book[@cat='none']", doc, XPathConstants.BOOLEAN);
        check(!f, "xp-boolean-false");

        // NODE
        Node node = (Node) xp.evaluate("//book[1]", doc, XPathConstants.NODE);
        eq(((Element) node).getAttribute("id"), "b1", "xp-node");

        // NODESET
        NodeList set = (NodeList) xp.evaluate("//book[@cat='fiction']", doc, XPathConstants.NODESET);
        eqi(set.getLength(), 2, "xp-nodeset-len");
        eq(((Element) set.item(0)).getAttribute("id"), "b1", "xp-nodeset-0");
        eq(((Element) set.item(1)).getAttribute("id"), "b3", "xp-nodeset-1");

        // compiled expression reuse
        XPathExpression ex = xp.compile("//book[price>15]/@id");
        NodeList ids = (NodeList) ex.evaluate(doc, XPathConstants.NODESET);
        eqi(ids.getLength(), 2, "xp-compiled-len");
        eq(ids.item(0).getNodeValue(), "b2", "xp-compiled-0");
        eq(ids.item(1).getNodeValue(), "b3", "xp-compiled-1");

        // evaluate over InputSource
        InputSource is = new InputSource(new StringReader("<a><b>1</b><b>2</b></a>"));
        eq(xp.evaluate("count(/a/b)", is), "2", "xp-inputsource");
    }

    // ---------------------------------------------------------------- XPath functions + axes
    static void xpathFunctionsAxes() throws Throwable {
        Document doc = library();
        XPath xp = XPathFactory.newInstance().newXPath();

        eq(xp.evaluate("local-name(//book[1])", doc), "book", "xpf-localname");
        eq(xp.evaluate("name(//book[1])", doc), "book", "xpf-name");
        eq(xp.evaluate("string(//book[1]/title)", doc), "Alpha", "xpf-string");
        eqi(d(xp, "string-length(//book[1]/title)", doc), 5, "xpf-strlen");
        check(b(xp, "starts-with(//book[1]/title, 'Al')", doc), "xpf-startswith");
        check(b(xp, "contains(//book[2]/title, 'et')", doc), "xpf-contains");
        eq(xp.evaluate("substring('hello', 2, 3)", doc), "ell", "xpf-substring");
        eq(xp.evaluate("normalize-space('  x   y ')", doc), "x y", "xpf-normalizespace");
        eq(xp.evaluate("concat('a','-','b')", doc), "a-b", "xpf-concat");
        eq(xp.evaluate("translate('abc','b','B')", doc), "aBc", "xpf-translate");
        eqi(d(xp, "string-length(translate('  a b ',' ',''))", doc), 2, "xpf-translate-strip");
        check(b(xp, "not(//book[@cat='none'])", doc), "xpf-not");

        // position / last / axes
        eq(xp.evaluate("//book[last()]/@id", doc), "b3", "xpa-last");
        eq(xp.evaluate("//book[position()=2]/@id", doc), "b2", "xpa-position");
        eqi(d(xp, "count(//book[1]/following-sibling::book)", doc), 2, "xpa-following");
        eqi(d(xp, "count(//book[3]/preceding-sibling::book)", doc), 2, "xpa-preceding");
        eqi(d(xp, "count(//title/ancestor::book)", doc), 3, "xpa-ancestor");
        eqi(d(xp, "count(//library/descendant::title)", doc), 3, "xpa-descendant");
        eqi(d(xp, "count(//book[1]/child::*)", doc), 2, "xpa-child");
        eqi(d(xp, "count(//book[1]/attribute::*)", doc), 2, "xpa-attribute");
        eq(xp.evaluate("//book[price=20]/@id", doc), "b2", "xpa-pricepredicate");
    }

    static int d(XPath xp, String e, Object src) throws Exception {
        return ((Double) xp.evaluate(e, src, XPathConstants.NUMBER)).intValue();
    }
    static boolean b(XPath xp, String e, Object src) throws Exception {
        return (Boolean) xp.evaluate(e, src, XPathConstants.BOOLEAN);
    }

    // ---------------------------------------------------------------- XPath NamespaceContext + variables
    static void xpathNamespaceVariable() throws Throwable {
        Document doc = parse("<r xmlns:m=\"urn:m\"><m:item>X</m:item><m:item>Y</m:item></r>", true);
        XPath xp = XPathFactory.newInstance().newXPath();
        xp.setNamespaceContext(new NamespaceContext() {
            public String getNamespaceURI(String prefix) { return "m".equals(prefix) ? "urn:m" : XMLConstants.NULL_NS_URI; }
            public String getPrefix(String uri) { return "urn:m".equals(uri) ? "m" : null; }
            public Iterator<String> getPrefixes(String uri) { return Collections.singletonList("m").iterator(); }
        });
        eq(xp.evaluate("/r/m:item[1]", doc), "X", "xpns-first");
        eqi(d(xp, "count(/r/m:item)", doc), 2, "xpns-count");

        // variable resolver
        Document lib = library();
        XPath xv = XPathFactory.newInstance().newXPath();
        xv.setXPathVariableResolver(qn -> "cat".equals(qn.getLocalPart()) ? "fiction" : null);
        eqi(d(xv, "count(//book[@cat=$cat])", lib), 2, "xpvar-count");
        eq(xv.evaluate("//book[@cat=$cat][1]/title", lib), "Alpha", "xpvar-title");
    }

    // ---------------------------------------------------------------- SAX
    static void saxParsing() throws Throwable {
        SAXParserFactory spf = SAXParserFactory.newInstance();
        spf.setNamespaceAware(true);
        SAXParser sp = spf.newSAXParser();
        check(sp.isNamespaceAware(), "sax-nsaware");

        final int[] starts = {0}, ends = {0}, chars = {0}, docs = {0};
        final List<String> elemNames = new ArrayList<>();
        final StringBuilder text = new StringBuilder();
        final String[] firstAttr = {null};
        DefaultHandler h = new DefaultHandler() {
            public void startDocument() { docs[0]++; }
            public void startElement(String uri, String local, String qn, Attributes at) {
                starts[0]++;
                elemNames.add(qn);
                if (at.getLength() > 0 && firstAttr[0] == null) firstAttr[0] = at.getQName(0) + "=" + at.getValue(0);
            }
            public void endElement(String uri, String local, String qn) { ends[0]++; }
            public void characters(char[] ch, int s, int len) { chars[0]++; text.append(ch, s, len); }
        };
        String xml = "<root><item id=\"1\">a</item><item id=\"2\">b</item></root>";
        sp.parse(new InputSource(new StringReader(xml)), h);
        eqi(docs[0], 1, "sax-startdoc");
        eqi(starts[0], 3, "sax-starts");
        eqi(ends[0], 3, "sax-ends");
        eq(text.toString(), "ab", "sax-chars-text");
        check(chars[0] >= 2, "sax-chars-count");
        eq(elemNames.get(0), "root", "sax-first-elem");
        eq(firstAttr[0], "id=1", "sax-attr");

        // namespace-aware reporting via XMLReader directly
        XMLReader reader = sp.getXMLReader();
        final String[] nsuri = {null}, local = {null};
        reader.setContentHandler(new DefaultHandler() {
            public void startElement(String uri, String l, String qn, Attributes at) {
                if (nsuri[0] == null) { nsuri[0] = uri; local[0] = l; }
            }
        });
        reader.parse(new InputSource(new StringReader("<n:e xmlns:n=\"urn:n\"/>")));
        eq(nsuri[0], "urn:n", "sax-xmlreader-uri");
        eq(local[0], "e", "sax-xmlreader-local");

        // Attributes index lookups
        final int[] attrLen = {-1};
        final String[] byqn = {null}, bylocal = {null};
        sp.getXMLReader().setContentHandler(new DefaultHandler() {
            public void startElement(String uri, String l, String qn, Attributes at) {
                if ("e".equals(l)) {
                    attrLen[0] = at.getLength();
                    byqn[0] = at.getValue("a");
                    bylocal[0] = at.getValue("urn:p", "b");
                }
            }
        });
        sp.getXMLReader().parse(new InputSource(new StringReader("<e xmlns:p=\"urn:p\" a=\"1\" p:b=\"2\"/>")));
        eqi(attrLen[0], 2, "sax-attrlen"); // ns-aware reader: xmlns:p is NOT a reported attribute (namespace-prefixes off) -> just a, p:b
        eq(byqn[0], "1", "sax-attr-byqn");
        eq(bylocal[0], "2", "sax-attr-bylocalns");
    }

    static void saxLexical() throws Throwable {
        SAXParserFactory spf = SAXParserFactory.newInstance();
        SAXParser sp = spf.newSAXParser();
        XMLReader reader = sp.getXMLReader();
        final List<String> comments = new ArrayList<>();
        final int[] cdataStart = {0}, cdataEnd = {0};
        DefaultHandler2 h2 = new DefaultHandler2() {
            public void comment(char[] ch, int s, int len) { comments.add(new String(ch, s, len)); }
            public void startCDATA() { cdataStart[0]++; }
            public void endCDATA() { cdataEnd[0]++; }
        };
        reader.setProperty("http://xml.org/sax/properties/lexical-handler", h2);
        reader.setContentHandler(h2);
        reader.parse(new InputSource(new StringReader("<r><!--hi--><![CDATA[xy]]></r>")));
        eqi(comments.size(), 1, "saxlex-comment-count");
        eq(comments.get(0), "hi", "saxlex-comment-text");
        eqi(cdataStart[0], 1, "saxlex-cdata-start");
        eqi(cdataEnd[0], 1, "saxlex-cdata-end");
    }

    // ---------------------------------------------------------------- StAX cursor
    static void staxCursor() throws Throwable {
        XMLInputFactory f = XMLInputFactory.newInstance();
        f.setProperty(XMLInputFactory.IS_COALESCING, Boolean.TRUE);
        f.setProperty(XMLInputFactory.SUPPORT_DTD, Boolean.FALSE);
        XMLStreamReader r = f.createXMLStreamReader(new StringReader("<a x=\"1\" y=\"2\"><b>hi</b><b>yo</b></a>"));
        int starts = 0, ends = 0;
        String firstText = null;
        int rootAttrs = -1;
        String attrX = null, attrXns = null;
        List<String> names = new ArrayList<>();
        while (r.hasNext()) {
            int ev = r.next();
            if (ev == XMLStreamConstants.START_ELEMENT) {
                starts++;
                names.add(r.getLocalName());
                if (r.getLocalName().equals("a")) {
                    rootAttrs = r.getAttributeCount();
                    attrX = r.getAttributeValue(0);
                    attrXns = r.getAttributeValue(null, "x");
                }
            } else if (ev == XMLStreamConstants.END_ELEMENT) {
                ends++;
            } else if (ev == XMLStreamConstants.CHARACTERS) {
                if (firstText == null) firstText = r.getText();
            }
        }
        r.close();
        eqi(starts, 3, "stax-cur-starts");
        eqi(ends, 3, "stax-cur-ends");
        eqi(rootAttrs, 2, "stax-cur-attrcount");
        eq(attrX, "1", "stax-cur-attr0");
        eq(attrXns, "1", "stax-cur-attrns");
        eq(firstText, "hi", "stax-cur-text");
        eq(names.get(0), "a", "stax-cur-firstname");
    }

    // ---------------------------------------------------------------- StAX events
    static void staxEvents() throws Throwable {
        XMLInputFactory f = XMLInputFactory.newInstance();
        XMLEventReader r = f.createXMLEventReader(new StringReader("<a><b id=\"5\">t</b></a>"));
        int starts = 0;
        String bId = null, charText = null;
        while (r.hasNext()) {
            XMLEvent e = r.nextEvent();
            if (e.isStartElement()) {
                starts++;
                StartElement se = e.asStartElement();
                if (se.getName().getLocalPart().equals("b")) {
                    Attribute at = se.getAttributeByName(new QName("id"));
                    bId = at.getValue();
                }
            } else if (e.isCharacters()) {
                String d = e.asCharacters().getData();
                if (!d.trim().isEmpty()) charText = d;
            }
        }
        r.close();
        eqi(starts, 2, "stax-evt-starts");
        eq(bId, "5", "stax-evt-attr");
        eq(charText, "t", "stax-evt-text");
    }

    // ---------------------------------------------------------------- StAX writer
    static void staxWriter() throws Throwable {
        XMLOutputFactory of = XMLOutputFactory.newInstance();
        StringWriter sw = new StringWriter();
        XMLStreamWriter w = of.createXMLStreamWriter(sw);
        w.writeStartElement("a");
        w.writeAttribute("x", "1");
        w.writeStartElement("b");
        w.writeCharacters("hi");
        w.writeEndElement();
        w.writeEmptyElement("c");
        w.writeEndElement();
        w.flush();
        w.close();
        eq(sw.toString(), "<a x=\"1\"><b>hi</b><c/></a>", "staxw-output");

        // round trip: parse what we wrote
        Document doc = parse(sw.toString(), false);
        eqi(doc.getElementsByTagName("b").getLength(), 1, "staxw-roundtrip-b");
        eq(doc.getElementsByTagName("b").item(0).getTextContent(), "hi", "staxw-roundtrip-text");
    }

    // ---------------------------------------------------------------- Transform identity / serialization
    static void transformIdentity() throws Throwable {
        Document d = builder(false).newDocument();
        Element r = d.createElement("r");
        r.setAttribute("a", "1");
        Element c = d.createElement("c");
        c.appendChild(d.createTextNode("x"));
        r.appendChild(c);
        d.appendChild(r);

        TransformerFactory tf = TransformerFactory.newInstance();
        Transformer t = tf.newTransformer();
        t.setOutputProperty(OutputKeys.OMIT_XML_DECLARATION, "yes");
        t.setOutputProperty(OutputKeys.METHOD, "xml");
        eq(t.getOutputProperty(OutputKeys.OMIT_XML_DECLARATION), "yes", "tr-getprop");
        StringWriter sw = new StringWriter();
        t.transform(new DOMSource(d), new StreamResult(sw));
        eq(sw.toString(), "<r a=\"1\"><c>x</c></r>", "tr-identity-serialize");

        // text method extracts text only
        Transformer tt = tf.newTransformer();
        tt.setOutputProperty(OutputKeys.METHOD, "text");
        StringWriter sw2 = new StringWriter();
        tt.transform(new DOMSource(d), new StreamResult(sw2));
        eq(sw2.toString(), "x", "tr-text-method");

        // DOMSource -> DOMResult clone
        Transformer tc = tf.newTransformer();
        DOMResult dr = new DOMResult();
        tc.transform(new DOMSource(d), dr);
        Document cloned = (Document) dr.getNode();
        eq(cloned.getDocumentElement().getNodeName(), "r", "tr-domresult");
    }

    static void transformSources() throws Throwable {
        TransformerFactory tf = TransformerFactory.newInstance();

        // StreamSource -> DOMResult (parse)
        Transformer t = tf.newTransformer();
        DOMResult dr = new DOMResult();
        t.transform(new StreamSource(new StringReader("<z><q>5</q></z>")), dr);
        Document doc = (Document) dr.getNode();
        eq(doc.getDocumentElement().getNodeName(), "z", "trs-stream-dom");
        eq(doc.getElementsByTagName("q").item(0).getTextContent(), "5", "trs-stream-q");

        // SAXSource -> StreamResult
        Transformer t2 = tf.newTransformer();
        t2.setOutputProperty(OutputKeys.OMIT_XML_DECLARATION, "yes");
        StringWriter sw = new StringWriter();
        t2.transform(new SAXSource(new InputSource(new StringReader("<p k=\"v\">w</p>"))), new StreamResult(sw));
        eq(sw.toString(), "<p k=\"v\">w</p>", "trs-saxsource");

        // DOMSource -> StreamResult via OutputStream
        Document d = parse("<o>data</o>", false);
        Transformer t3 = tf.newTransformer();
        t3.setOutputProperty(OutputKeys.OMIT_XML_DECLARATION, "yes");
        ByteArrayOutputStream bos = new ByteArrayOutputStream();
        t3.transform(new DOMSource(d), new StreamResult(bos));
        eq(new String(bos.toByteArray(), "UTF-8"), "<o>data</o>", "trs-bytestream");
    }

    static void transformXslt() throws Throwable {
        String xslt = "<?xml version=\"1.0\"?>"
            + "<xsl:stylesheet version=\"1.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">"
            + "<xsl:output method=\"text\"/>"
            + "<xsl:template match=\"/\">count=<xsl:value-of select=\"count(//item)\"/></xsl:template>"
            + "</xsl:stylesheet>";
        TransformerFactory tf = TransformerFactory.newInstance();
        Templates templates = tf.newTemplates(new StreamSource(new StringReader(xslt)));
        Transformer t = templates.newTransformer();
        String data = "<root><item/><item/><item/></root>";
        StringWriter sw = new StringWriter();
        t.transform(new StreamSource(new StringReader(data)), new StreamResult(sw));
        eq(sw.toString(), "count=3", "xslt-count");

        // second stylesheet: copy + uppercase-ish via value-of of a specific node
        String xslt2 = "<?xml version=\"1.0\"?>"
            + "<xsl:stylesheet version=\"1.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">"
            + "<xsl:output method=\"text\"/>"
            + "<xsl:template match=\"/\"><xsl:value-of select=\"sum(//n)\"/></xsl:template>"
            + "</xsl:stylesheet>";
        Transformer t2 = tf.newTransformer(new StreamSource(new StringReader(xslt2)));
        StringWriter sw2 = new StringWriter();
        t2.transform(new StreamSource(new StringReader("<r><n>4</n><n>6</n></r>")), new StreamResult(sw2));
        eq(sw2.toString(), "10", "xslt-sum");
    }

    // ---------------------------------------------------------------- XSD validation
    static void validation() throws Throwable {
        String xsd = "<?xml version=\"1.0\"?>"
            + "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">"
            + "<xs:element name=\"note\"><xs:complexType><xs:sequence>"
            + "<xs:element name=\"to\" type=\"xs:string\"/>"
            + "<xs:element name=\"n\" type=\"xs:int\"/>"
            + "</xs:sequence></xs:complexType></xs:element>"
            + "</xs:schema>";
        SchemaFactory sf = SchemaFactory.newInstance(XMLConstants.W3C_XML_SCHEMA_NS_URI);
        Schema schema = sf.newSchema(new StreamSource(new StringReader(xsd)));

        // valid
        Validator v1 = schema.newValidator();
        v1.setErrorHandler(RETHROW);
        v1.validate(new StreamSource(new StringReader("<note><to>x</to><n>5</n></note>")));
        ok++; // no exception => valid passed
        check(true, "xsd-valid"); // explicit marker line

        // invalid: n not an int
        Validator v2 = schema.newValidator();
        v2.setErrorHandler(RETHROW);
        expect(SAXException.class,
            () -> v2.validate(new StreamSource(new StringReader("<note><to>x</to><n>abc</n></note>"))),
            "xsd-invalid-type");

        // invalid: missing required element
        Validator v3 = schema.newValidator();
        v3.setErrorHandler(RETHROW);
        expect(SAXException.class,
            () -> v3.validate(new StreamSource(new StringReader("<note><to>x</to></note>"))),
            "xsd-invalid-missing");

        // validate a DOMSource too
        Document doc = parse("<note><to>y</to><n>9</n></note>", false);
        Validator v4 = schema.newValidator();
        v4.setErrorHandler(RETHROW);
        v4.validate(new DOMSource(doc));
        check(true, "xsd-valid-domsource");
    }

    // ---------------------------------------------------------------- javax.xml.datatype
    static void datatype() throws Throwable {
        DatatypeFactory df = DatatypeFactory.newInstance();

        XMLGregorianCalendar c = df.newXMLGregorianCalendar("2021-03-15T10:20:30");
        eqi(c.getYear(), 2021, "dt-year");
        eqi(c.getMonth(), 3, "dt-month");
        eqi(c.getDay(), 15, "dt-day");
        eqi(c.getHour(), 10, "dt-hour");
        eqi(c.getMinute(), 20, "dt-minute");
        eqi(c.getSecond(), 30, "dt-second");
        eq(c.getXMLSchemaType(), DatatypeConstants.DATETIME, "dt-schematype");

        XMLGregorianCalendar donly = df.newXMLGregorianCalendarDate(2020, 2, 29, DatatypeConstants.FIELD_UNDEFINED);
        eqi(donly.getDay(), 29, "dt-dateonly-day");
        eq(donly.getXMLSchemaType(), DatatypeConstants.DATE, "dt-dateonly-type");

        Duration d = df.newDuration("P1Y2M3DT4H5M6S");
        eqi(d.getYears(), 1, "dt-dur-years");
        eqi(d.getMonths(), 2, "dt-dur-months");
        eqi(d.getDays(), 3, "dt-dur-days");
        eqi(d.getHours(), 4, "dt-dur-hours");
        eqi(d.getMinutes(), 5, "dt-dur-minutes");
        eqi(d.getSeconds(), 6, "dt-dur-seconds");
        eqi(d.getSign(), 1, "dt-dur-sign");

        Duration y1 = df.newDuration("P1Y");
        Duration m11 = df.newDuration("P11M");
        eqi(y1.compare(m11), DatatypeConstants.GREATER, "dt-dur-compare");

        Duration neg = df.newDuration(false, 0, 0, 1, 0, 0, 0);
        eqi(neg.getSign(), -1, "dt-dur-negsign");
        eqi(neg.getDays(), 1, "dt-dur-negdays");

        eq(df.newDuration("P2D").toString(), "P2D", "dt-dur-tostring");
    }

    // ---------------------------------------------------------------- QName + XMLConstants
    static void qnameAndConstants() throws Throwable {
        QName q = new QName("urn:ns", "local", "p");
        eq(q.getNamespaceURI(), "urn:ns", "qn-uri");
        eq(q.getLocalPart(), "local", "qn-local");
        eq(q.getPrefix(), "p", "qn-prefix");
        eq(q.toString(), "{urn:ns}local", "qn-tostring");

        QName parsed = QName.valueOf("{urn:ns}local");
        eq(parsed.getLocalPart(), "local", "qn-valueof-local");
        eq(parsed.getNamespaceURI(), "urn:ns", "qn-valueof-uri");

        QName noNs = new QName("bare");
        eq(noNs.getNamespaceURI(), XMLConstants.NULL_NS_URI, "qn-bare-ns");
        eq(noNs.getPrefix(), XMLConstants.DEFAULT_NS_PREFIX, "qn-bare-prefix");

        // equals ignores prefix
        check(q.equals(new QName("urn:ns", "local")), "qn-equals-ignores-prefix");
        check(!q.equals(new QName("urn:other", "local")), "qn-notequals-ns");
        eqi(q.hashCode(), new QName("urn:ns", "local", "other").hashCode(), "qn-hashcode");

        // XMLConstants
        eq(XMLConstants.W3C_XML_SCHEMA_NS_URI, "http://www.w3.org/2001/XMLSchema", "xc-xsd-ns");
        eq(XMLConstants.XML_NS_URI, "http://www.w3.org/XML/1998/namespace", "xc-xml-ns");
        eq(XMLConstants.XMLNS_ATTRIBUTE_NS_URI, "http://www.w3.org/2000/xmlns/", "xc-xmlns-ns");
        eq(XMLConstants.XML_NS_PREFIX, "xml", "xc-xml-prefix");
        eq(XMLConstants.XMLNS_ATTRIBUTE, "xmlns", "xc-xmlns-attr");
        eq(XMLConstants.NULL_NS_URI, "", "xc-null-ns");
        eq(XMLConstants.DEFAULT_NS_PREFIX, "", "xc-default-prefix");
    }

    // ---------------------------------------------------------------- error paths
    static void errorPaths() throws Throwable {
        // malformed XML -> SAXParseException
        expect(SAXParseException.class, () -> parse("<root><a></root>", false), "err-malformed");
        expect(SAXParseException.class, () -> parse("<root", false), "err-unclosed");
        expect(SAXParseException.class, () -> parse("not xml at all", false), "err-notxml");

        // empty document
        expect(SAXParseException.class, () -> parse("", false), "err-empty");

        // bad XPath compile
        XPath xp = XPathFactory.newInstance().newXPath();
        expect(XPathExpressionException.class, () -> xp.compile("///["), "err-xpath-compile");

        // StAX over malformed -> XMLStreamException during iteration
        expect(XMLStreamException.class, () -> {
            XMLStreamReader r = XMLInputFactory.newInstance().createXMLStreamReader(new StringReader("<a><b></a>"));
            while (r.hasNext()) r.next();
        }, "err-stax-malformed");

        // unknown SAX property
        expect(SAXException.class, () -> {
            XMLReader reader = SAXParserFactory.newInstance().newSAXParser().getXMLReader();
            reader.setProperty("http://example.com/unknown-property", "x");
        }, "err-sax-badprop");
    }

    // ---------------------------------------------------------------- shared fixture
    static Document library() throws Exception {
        String xml = "<library>"
            + "<book id=\"b1\" cat=\"fiction\"><title>Alpha</title><price>10</price></book>"
            + "<book id=\"b2\" cat=\"tech\"><title>Beta</title><price>20</price></book>"
            + "<book id=\"b3\" cat=\"fiction\"><title>Gamma</title><price>30</price></book>"
            + "</library>";
        return parse(xml, false);
    }
}
