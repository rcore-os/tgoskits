#!/usr/bin/env python3
# NetworkxCarpet.py - deep closed-form-assertion carpet for NetworkX on musl-native CPython.
#
# Exercises the graph-algorithm surface against hand-computable reference graphs so every result
# is an exact structural value or a closed-form scalar: construction (Graph / DiGraph / MultiGraph /
# from_edgelist / from_numpy_array), degree and neighbours, shortest paths (dijkstra / bellman_ford /
# astar / floyd_warshall / single-source BFS), BFS/DFS traversal order, connected and strongly
# connected components, centrality (degree / betweenness / closeness / eigenvector), PageRank,
# minimum spanning tree (kruskal / prim), topological sort, cliques and matching, classic generators
# (complete / cycle / path / grid / karate club), and adjacency / Laplacian spectral structure.
#
# Reference graphs are chosen so the answers are symmetry- or hand-derivable (a 3-cycle has uniform
# PageRank 1/3, the middle of a 3-path has betweenness 1.0 and closeness 1.0, K4 has clustering and
# transitivity 1.0, ...). Integer and set-valued results are compared exactly; floats within 1e-6
# relative tolerance, so the host reference and a newer musl target build agree. Self-contained
# ok/fail counters; prints NETWORKX_RESULT then NETWORKX_DONE only when fail == 0.
import sys

ok = 0
fail = 0


def chk(name, cond, info=""):
    global ok, fail
    if cond:
        ok += 1
        print("  ok %s%s" % (name, (" " + info) if info else ""))
    else:
        fail += 1
        print("  FAIL %s%s" % (name, (" " + info) if info else ""))


import numpy as np
import networkx as nx

chk("version", int(nx.__version__.split(".")[0]) >= 2, "networkx=%s" % nx.__version__)

# ---------------------------------------------------------------- construction
G = nx.Graph()
G.add_nodes_from([0, 1, 2, 3])
G.add_edges_from([(0, 1), (1, 2), (2, 3)])
chk("graph_order", G.number_of_nodes() == 4)
chk("graph_size", G.number_of_edges() == 3)
chk("graph_has_edge", G.has_edge(1, 2) and not G.has_edge(0, 3))
chk("graph_undirected", not G.is_directed())

D = nx.DiGraph([(0, 1), (0, 2), (1, 3), (2, 3)])
chk("digraph_directed", D.is_directed())
chk("digraph_arc_asym", D.has_edge(0, 1) and not D.has_edge(1, 0))
chk("digraph_size", D.number_of_edges() == 4)

MG = nx.MultiGraph()
MG.add_edge(0, 1)
MG.add_edge(0, 1)
MG.add_edge(1, 2)
chk("multigraph_parallel", MG.number_of_edges(0, 1) == 2)
chk("multigraph_total", MG.number_of_edges() == 3)

# from_edgelist and from_numpy_array reconstruct the same 2-edge structure.
Ge = nx.from_edgelist([(0, 1), (1, 2)])
chk("from_edgelist", Ge.number_of_edges() == 2 and Ge.has_edge(0, 1) and Ge.has_edge(1, 2))
Gn = nx.from_numpy_array(np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]]))
chk("from_numpy_array", Gn.number_of_edges() == 2 and Gn.has_edge(0, 1) and Gn.has_edge(1, 2))

# ---------------------------------------------------------------- degree / neighbours
star = nx.star_graph(4)  # centre 0 joined to 1..4
chk("degree_center", star.degree(0) == 4)
chk("degree_leaf", star.degree(1) == 1)
chk("neighbors", sorted(star.neighbors(0)) == [1, 2, 3, 4])
chk("degree_sum_handshake", sum(d for _, d in G.degree()) == 2 * G.number_of_edges())
Dd = nx.DiGraph([(0, 1), (0, 2), (3, 0)])
chk("in_out_degree", Dd.out_degree(0) == 2 and Dd.in_degree(0) == 1)

# ---------------------------------------------------------------- shortest paths
Gw = nx.DiGraph()
Gw.add_weighted_edges_from([(0, 1, 1.0), (1, 2, 2.0), (0, 2, 10.0)])
chk("dijkstra_len", abs(nx.dijkstra_path_length(Gw, 0, 2) - 3.0) < 1e-9)
chk("dijkstra_path", nx.dijkstra_path(Gw, 0, 2) == [0, 1, 2])
chk("bellman_ford_len", abs(nx.bellman_ford_path_length(Gw, 0, 2) - 3.0) < 1e-9)
chk("bellman_ford_path", nx.bellman_ford_path(Gw, 0, 2) == [0, 1, 2])

Ga = nx.Graph()
Ga.add_weighted_edges_from([(0, 1, 1.0), (1, 2, 1.0), (0, 2, 3.0)])
chk("astar_len", abs(nx.astar_path_length(Ga, 0, 2) - 2.0) < 1e-9)
chk("astar_path", nx.astar_path(Ga, 0, 2) == [0, 1, 2])

# Floyd-Warshall all-pairs matrix on the weighted digraph reproduces the dijkstra distance.
fw = dict(nx.floyd_warshall(Gw))
chk("floyd_warshall_02", abs(fw[0][2] - 3.0) < 1e-9)
chk("floyd_warshall_self", abs(fw[0][0]) < 1e-12)

# BFS shortest-path lengths on an unweighted path are the index offsets.
ssl = dict(nx.single_source_shortest_path_length(nx.path_graph(4), 0))
chk("single_source_bfs", ssl == {0: 0, 1: 1, 2: 2, 3: 3})
chk("shortest_path_len_unweighted", nx.shortest_path_length(nx.path_graph(5), 0, 4) == 4)
chk("has_path", nx.has_path(nx.path_graph(3), 0, 2) and not nx.has_path(nx.Graph([(0, 1), (2, 3)]), 0, 2))

# ---------------------------------------------------------------- BFS / DFS traversal
T = nx.path_graph(4)
chk("bfs_edges", list(nx.bfs_edges(T, 0)) == [(0, 1), (1, 2), (2, 3)])
chk("dfs_edges", list(nx.dfs_edges(T, 0)) == [(0, 1), (1, 2), (2, 3)])
chk("dfs_preorder", list(nx.dfs_preorder_nodes(T, 0)) == [0, 1, 2, 3])
# On a star the BFS tree from the centre reaches every leaf in one hop.
bt = nx.bfs_tree(nx.star_graph(3), 0)
chk("bfs_tree", sorted(bt.edges()) == [(0, 1), (0, 2), (0, 3)])

# ---------------------------------------------------------------- connectivity
U = nx.Graph([(0, 1), (2, 3)])
cc = sorted(sorted(c) for c in nx.connected_components(U))
chk("connected_components", cc == [[0, 1], [2, 3]])
chk("num_connected_components", nx.number_connected_components(U) == 2)
chk("is_connected", nx.is_connected(nx.path_graph(4)) and not nx.is_connected(U))

Sd = nx.DiGraph([(0, 1), (1, 0), (1, 2), (2, 3), (3, 2)])
scc = sorted(sorted(c) for c in nx.strongly_connected_components(Sd))
chk("strongly_connected_components", scc == [[0, 1], [2, 3]])
chk("num_scc", nx.number_strongly_connected_components(Sd) == 2)

# ---------------------------------------------------------------- centrality
P3 = nx.path_graph(3)
deg_c = nx.degree_centrality(P3)
chk("degree_centrality", abs(deg_c[1] - 1.0) < 1e-9 and abs(deg_c[0] - 0.5) < 1e-9)
bc = nx.betweenness_centrality(P3)
chk("betweenness_center", abs(bc[1] - 1.0) < 1e-9 and abs(bc[0]) < 1e-12)
cc_cen = nx.closeness_centrality(P3)
chk("closeness_center", abs(cc_cen[1] - 1.0) < 1e-9 and abs(cc_cen[0] - 2.0 / 3.0) < 1e-9)
# The 4-cycle is vertex-transitive: eigenvector centrality is uniform across nodes.
ec = nx.eigenvector_centrality_numpy(nx.cycle_graph(4))
chk("eigenvector_uniform", max(ec.values()) - min(ec.values()) < 1e-6)
# K4 is fully symmetric: every degree/betweenness/closeness value is identical.
kbc = nx.betweenness_centrality(nx.complete_graph(4))
chk("betweenness_complete_zero", all(abs(v) < 1e-12 for v in kbc.values()))

# ---------------------------------------------------------------- PageRank
pr = nx.pagerank(nx.cycle_graph(3), alpha=0.85)
chk("pagerank_uniform", all(abs(v - 1.0 / 3.0) < 1e-6 for v in pr.values()))
chk("pagerank_sums_to_one", abs(sum(pr.values()) - 1.0) < 1e-9)
# On a directed 2-cycle PageRank is also uniform by symmetry.
pr2 = nx.pagerank(nx.DiGraph([(0, 1), (1, 0)]), alpha=0.85)
chk("pagerank_dicycle", abs(pr2[0] - 0.5) < 1e-6 and abs(pr2[1] - 0.5) < 1e-6)

# ---------------------------------------------------------------- minimum spanning tree
Gm = nx.Graph()
Gm.add_weighted_edges_from([(0, 1, 1.0), (1, 2, 2.0), (0, 2, 3.0), (2, 3, 4.0)])
mst = nx.minimum_spanning_tree(Gm)
chk("mst_total_weight", abs(mst.size(weight="weight") - 7.0) < 1e-9)
chk("mst_edge_count", mst.number_of_edges() == Gm.number_of_nodes() - 1)
kr = sorted(nx.minimum_spanning_edges(Gm, algorithm="kruskal", data=False))
chk("mst_kruskal_edges", kr == [(0, 1), (1, 2), (2, 3)])
pm = sorted(tuple(sorted(e)) for e in nx.minimum_spanning_edges(Gm, algorithm="prim", data=False))
chk("mst_prim_edges", pm == [(0, 1), (1, 2), (2, 3)])

# ---------------------------------------------------------------- topological sort
dag = nx.DiGraph([(0, 1), (0, 2), (1, 3), (2, 3)])
chk("is_dag", nx.is_directed_acyclic_graph(dag))
topo = list(nx.topological_sort(dag))
# A valid topological order places the tail of every arc before its head.
chk("topological_sort", all(topo.index(u) < topo.index(v) for u, v in dag.edges()))
chk("cycle_not_dag", not nx.is_directed_acyclic_graph(nx.DiGraph([(0, 1), (1, 0)])))

# ---------------------------------------------------------------- cliques / matching
K4 = nx.complete_graph(4)
cliques = list(nx.find_cliques(K4))
chk("max_clique_size", max(len(c) for c in cliques) == 4)
chk("clique_is_whole", len(cliques) == 1 and sorted(cliques[0]) == [0, 1, 2, 3])
Gmatch = nx.Graph([(0, 1), (2, 3), (1, 2)])
matching = nx.max_weight_matching(Gmatch)
chk("max_weight_matching", len(matching) == 2)
mm = nx.maximal_matching(nx.path_graph(4))
chk("maximal_matching_bound", 1 <= len(mm) <= 2)

# ---------------------------------------------------------------- generators
chk("complete_graph", nx.complete_graph(5).number_of_edges() == 10)  # C(5,2)
chk("cycle_graph", nx.cycle_graph(6).number_of_edges() == 6)
chk("path_graph", nx.path_graph(6).number_of_edges() == 5)
grid = nx.grid_2d_graph(2, 3)
chk("grid_2d_graph", grid.number_of_nodes() == 6 and grid.number_of_edges() == 7)
kc = nx.karate_club_graph()
chk("karate_club_graph", kc.number_of_nodes() == 34 and kc.number_of_edges() == 78)

# ---------------------------------------------------------------- adjacency / Laplacian
A = nx.adjacency_matrix(nx.path_graph(3)).toarray()
chk("adjacency_matrix", A.tolist() == [[0, 1, 0], [1, 0, 1], [0, 1, 0]])
chk("adjacency_symmetric", np.array_equal(A, A.T))
L = nx.laplacian_matrix(nx.path_graph(3)).toarray()
chk("laplacian_matrix", L.tolist() == [[1, -1, 0], [-1, 2, -1], [0, -1, 1]])
# Every graph Laplacian has a zero eigenvalue with the all-ones eigenvector; row sums are zero.
chk("laplacian_row_sum_zero", np.allclose(L.sum(axis=1), 0.0))
# Number of near-zero Laplacian eigenvalues equals the number of connected components (here 2).
Ldis = nx.laplacian_matrix(nx.Graph([(0, 1), (2, 3)])).toarray().astype(float)
eig = np.sort(np.linalg.eigvalsh(Ldis))
chk("laplacian_spectral_components", int(np.sum(np.abs(eig) < 1e-9)) == 2)

# ---------------------------------------------------------------- structural metrics
chk("density_complete", abs(nx.density(K4) - 1.0) < 1e-12)
chk("density_empty", abs(nx.density(nx.empty_graph(4))) < 1e-12)
chk("diameter_path", nx.diameter(nx.path_graph(4)) == 3)
chk("radius_path", nx.radius(nx.path_graph(5)) == 2)
chk("clustering_complete", abs(nx.average_clustering(K4) - 1.0) < 1e-12)
chk("transitivity_complete", abs(nx.transitivity(K4) - 1.0) < 1e-12)
chk("triangles_complete", nx.triangles(K4) == {0: 3, 1: 3, 2: 3, 3: 3})
chk("triangles_path_zero", set(nx.triangles(nx.path_graph(4)).values()) == {0})

# ================================================================ SUPPLEMENT ================================================================
# Full-API supplement: every must-add submodule/API from the audit gap brief, each with a
# hand-derivable / roundtrip / invariant assertion (never a guessed value).

# ---------------------------------------------------------------- graph_types + attributes
MD = nx.MultiDiGraph()
MD.add_edge(0, 1)
MD.add_edge(0, 1)
chk("multidigraph_parallel", MD.number_of_edges(0, 1) == 2)
chk("multidigraph_directed", MD.is_directed() and MD.is_multigraph())
chk("multidigraph_arc_dir", MD.has_edge(0, 1) and not MD.has_edge(1, 0))

Psub = nx.path_graph(5)
sub = Psub.subgraph([0, 1, 2])
chk("subgraph_order", sub.number_of_nodes() == 3 and sub.number_of_edges() == 2)

Gcp = G.copy()
chk("copy_equal", Gcp.number_of_nodes() == G.number_of_nodes() and Gcp.number_of_edges() == G.number_of_edges())
chk("copy_distinct", Gcp is not G)

Dtu = nx.DiGraph([(0, 1), (1, 0), (1, 2)])
chk("to_undirected", Dtu.to_undirected().number_of_edges() == 2)
Gtd = nx.path_graph(3).to_directed()
chk("to_directed", Gtd.is_directed() and Gtd.number_of_edges() == 4)

Gattr = nx.Graph()
Gattr.add_node(0)
nx.set_node_attributes(Gattr, {0: "red"}, name="color")
chk("set_get_node_attr", nx.get_node_attributes(Gattr, "color") == {0: "red"})
Gattr.add_edge(0, 1)
nx.set_edge_attributes(Gattr, {(0, 1): 5}, name="w")
chk("set_get_edge_attr", nx.get_edge_attributes(Gattr, "w") == {(0, 1): 5})

Gwd = nx.Graph()
Gwd.add_edge(0, 1, weight=2.5)
edata = list(Gwd.edges(data=True))
chk("edges_data", edata == [(0, 1, {"weight": 2.5})])
ndata = list(nx.path_graph(2).nodes(data=True))
chk("nodes_data", ndata == [(0, {}), (1, {})])

Padj = nx.path_graph(3)
chk("adj_neighbors", sorted(Padj.adj[1]) == [0, 2])
adjd = {n: sorted(nbrs) for n, nbrs in Padj.adjacency()}
chk("adjacency_iter", adjd == {0: [1], 1: [0, 2], 2: [1]})

# ---------------------------------------------------------------- generators (extra)
wg = nx.wheel_graph(5)  # hub + 4-cycle rim: 4 spokes + 4 rim edges
chk("wheel_graph", wg.number_of_nodes() == 5 and wg.number_of_edges() == 8)
chk("star_graph_gen", nx.star_graph(4).number_of_edges() == 4)
bt2 = nx.balanced_tree(2, 2)  # complete binary tree depth 2: 1+2+4=7 nodes, 6 edges
chk("balanced_tree", bt2.number_of_nodes() == 7 and bt2.number_of_edges() == 6)
cbg = nx.complete_bipartite_graph(2, 3)
chk("complete_bipartite", cbg.number_of_nodes() == 5 and cbg.number_of_edges() == 6)
chk("empty_graph_gen", nx.empty_graph(5).number_of_nodes() == 5 and nx.empty_graph(5).number_of_edges() == 0)
chk("trivial_graph", nx.trivial_graph().number_of_nodes() == 1)
lg = nx.lollipop_graph(4, 3)  # K4 (4 nodes, 6 edges) + path of 3 extra nodes (3 edges)
chk("lollipop_graph", lg.number_of_nodes() == 7 and lg.number_of_edges() == 9)
bb = nx.barbell_graph(3, 1)  # two K3 (3 edges each) + 1-node path bridge (2 edges)
chk("barbell_graph", bb.number_of_nodes() == 7 and bb.number_of_edges() == 8)

# Seeded random generators are deterministic; assert count/degree invariants (not exact structure).
gnp_full = nx.gnp_random_graph(10, 1.0, seed=1)
chk("gnp_complete", gnp_full.number_of_edges() == 45)  # p=1 -> complete K10
gnp_empty = nx.gnp_random_graph(10, 0.0, seed=1)
chk("gnp_empty", gnp_empty.number_of_edges() == 0)
er = nx.erdos_renyi_graph(8, 1.0, seed=1)
chk("erdos_renyi_alias", er.number_of_edges() == 28)  # complete K8
ba = nx.barabasi_albert_graph(20, 3, seed=1)
chk("barabasi_albert", ba.number_of_nodes() == 20 and ba.number_of_edges() == 3 * (20 - 3))
ws = nx.watts_strogatz_graph(10, 4, 0.0, seed=1)  # beta=0 -> pure ring lattice, degree k=4
chk("watts_strogatz", ws.number_of_edges() == 20 and all(d == 4 for _, d in ws.degree()))
rr = nx.random_regular_graph(3, 10, seed=1)
chk("random_regular", rr.number_of_nodes() == 10 and all(d == 3 for _, d in rr.degree()))

# ---------------------------------------------------------------- connectivity (extra)
Dwc = nx.DiGraph([(0, 1), (2, 3)])
wcc = sorted(sorted(c) for c in nx.weakly_connected_components(Dwc))
chk("weakly_connected_components", wcc == [[0, 1], [2, 3]])
chk("num_wcc", nx.number_weakly_connected_components(Dwc) == 2)
chk("is_weakly_connected", nx.is_weakly_connected(nx.DiGraph([(0, 1), (1, 2)])) and not nx.is_weakly_connected(Dwc))
chk("is_strongly_connected", nx.is_strongly_connected(nx.DiGraph([(0, 1), (1, 2), (2, 0)]))
    and not nx.is_strongly_connected(nx.DiGraph([(0, 1), (1, 2)])))
chk("node_connectivity", nx.node_connectivity(nx.complete_graph(4)) == 3)
chk("edge_connectivity_cycle", nx.edge_connectivity(nx.cycle_graph(5)) == 2)
chk("edge_connectivity_path", nx.edge_connectivity(nx.path_graph(4)) == 1)
ap = set(nx.articulation_points(nx.path_graph(4)))
chk("articulation_points", ap == {1, 2})
br = sorted(tuple(sorted(e)) for e in nx.bridges(nx.path_graph(4)))
chk("bridges_path", br == [(0, 1), (1, 2), (2, 3)])
chk("has_bridges", nx.has_bridges(nx.path_graph(4)) and not nx.has_bridges(nx.cycle_graph(4)))
cond = nx.condensation(nx.DiGraph([(0, 1), (1, 0)]))
chk("condensation", cond.number_of_nodes() == 1)

# ---------------------------------------------------------------- centrality (extra)
katz = nx.katz_centrality_numpy(nx.cycle_graph(4))
chk("katz_uniform", max(katz.values()) - min(katz.values()) < 1e-6)
harm = nx.harmonic_centrality(nx.path_graph(3))
chk("harmonic_center", harm[1] > harm[0] and harm[1] > harm[2])
# A 3-node path 0-1-2 has two symmetric edges with EQUAL betweenness, so use a
# 4-node path where the middle edge (1,2) carries strictly more shortest paths
# (4/6) than an end edge (0,1) (3/6).
ebc = nx.edge_betweenness_centrality(nx.path_graph(4))
mid = ebc[(1, 2)] if (1, 2) in ebc else ebc[(2, 1)]
end = ebc[(0, 1)] if (0, 1) in ebc else ebc[(1, 0)]
chk("edge_betweenness", mid > end and abs(end - 0.5) < 1e-9 and abs(mid - 4.0 / 6.0) < 1e-9)
Dstar = nx.DiGraph([(0, 1), (0, 2), (0, 3)])
idc = nx.in_degree_centrality(Dstar)
odc = nx.out_degree_centrality(Dstar)
chk("in_degree_centrality", abs(idc[1] - 1.0 / 3.0) < 1e-9 and abs(idc[0]) < 1e-12)
chk("out_degree_centrality", abs(odc[0] - 1.0) < 1e-9)
ecp = nx.eigenvector_centrality(nx.cycle_graph(4), max_iter=1000)
chk("eigenvector_power_uniform", max(ecp.values()) - min(ecp.values()) < 1e-4)
lc = nx.load_centrality(nx.path_graph(3))
chk("load_center", abs(lc[1] - 1.0) < 1e-9)

# ---------------------------------------------------------------- link_analysis
hubs, auth = nx.hits(nx.cycle_graph(3), max_iter=1000)
chk("hits_hubs_uniform", all(abs(v - 1.0 / 3.0) < 1e-4 for v in hubs.values()))
chk("hits_auth_uniform", all(abs(v - 1.0 / 3.0) < 1e-4 for v in auth.values()))
chk("hits_sums", abs(sum(hubs.values()) - 1.0) < 1e-6 and abs(sum(auth.values()) - 1.0) < 1e-6)
gm = nx.google_matrix(nx.cycle_graph(3))
chk("google_matrix_rowsum", np.allclose(np.asarray(gm).sum(axis=1), 1.0))
# Personalization concentrating all restart mass on node 0 skews PageRank toward it.
prp = nx.pagerank(nx.DiGraph([(0, 1), (1, 2), (2, 0)]), alpha=0.85, personalization={0: 1, 1: 0, 2: 0})
chk("pagerank_personalized", prp[0] > prp[1] and prp[0] > prp[2] and abs(sum(prp.values()) - 1.0) < 1e-6)

# ---------------------------------------------------------------- clustering (extra)
clus_path = nx.clustering(nx.path_graph(4))
chk("clustering_path_zero", all(abs(v) < 1e-12 for v in clus_path.values()))
clus_k4 = nx.clustering(K4)
chk("clustering_complete_one", all(abs(v - 1.0) < 1e-12 for v in clus_k4.values()))
# C4 (square) is bipartite with squares: the corner square-clustering is 1.0.
sq = nx.square_clustering(nx.cycle_graph(4))
chk("square_clustering", all(abs(v - 1.0) < 1e-9 for v in sq.values()))
gd = nx.generalized_degree(K4)
chk("generalized_degree", isinstance(gd[0], dict) and sum(gd[0].values()) == K4.degree(0))
chk("avg_clustering_cycle_zero", abs(nx.average_clustering(nx.cycle_graph(5))) < 1e-12)

# ---------------------------------------------------------------- community
# Two triangles joined by a single bridge edge: clear 2-community structure.
two_clique = nx.Graph([(0, 1), (1, 2), (0, 2), (3, 4), (4, 5), (3, 5), (2, 3)])
gmc = nx.community.greedy_modularity_communities(two_clique)
chk("greedy_modularity", len(gmc) == 2 and set().union(*[set(c) for c in gmc]) == set(range(6)))
lvc = nx.community.louvain_communities(nx.karate_club_graph(), seed=1)
chk("louvain_communities", len(lvc) >= 2 and set().union(*[set(c) for c in lvc]) == set(range(34)))
lpc = list(nx.community.label_propagation_communities(two_clique))
chk("label_propagation", set().union(*[set(c) for c in lpc]) == set(range(6)))
part = [{0, 1, 2}, {3, 4, 5}]
chk("modularity_positive", nx.community.modularity(two_clique, part) > 0)
gn = nx.community.girvan_newman(two_clique)
first_split = tuple(sorted(c) for c in next(gn))
chk("girvan_newman", sorted(map(sorted, first_split)) == [[0, 1, 2], [3, 4, 5]])
klb = nx.community.kernighan_lin_bisection(two_clique, seed=1)
chk("kernighan_lin", len(klb) == 2 and len(klb[0]) + len(klb[1]) == 6)

# ---------------------------------------------------------------- flow
# Diamond: s=0 -> {1,2} -> t=3 with unit capacities. Two disjoint paths -> max flow 2.
Fg = nx.DiGraph()
Fg.add_edge(0, 1, capacity=1.0)
Fg.add_edge(0, 2, capacity=1.0)
Fg.add_edge(1, 3, capacity=1.0)
Fg.add_edge(2, 3, capacity=1.0)
mfv = nx.maximum_flow_value(Fg, 0, 3)
chk("maximum_flow_value", abs(mfv - 2.0) < 1e-9)
flow_val, flow_dict = nx.maximum_flow(Fg, 0, 3)
chk("maximum_flow_value2", abs(flow_val - 2.0) < 1e-9)
# Flow conservation at internal node 1: inflow == outflow.
in1 = flow_dict[0].get(1, 0)
out1 = sum(flow_dict[1].values())
chk("flow_conservation", abs(in1 - out1) < 1e-9)
mcv = nx.minimum_cut_value(Fg, 0, 3)
chk("min_cut_equals_max_flow", abs(mcv - mfv) < 1e-9)
cut_val, (reach, nonreach) = nx.minimum_cut(Fg, 0, 3)
chk("minimum_cut_partition", abs(cut_val - 2.0) < 1e-9 and 0 in reach and 3 in nonreach)
# Cross-check alternative flow algorithms agree on the value.
chk("edmonds_karp", abs(nx.maximum_flow_value(Fg, 0, 3, flow_func=nx.algorithms.flow.edmonds_karp) - 2.0) < 1e-9)
chk("preflow_push", abs(nx.maximum_flow_value(Fg, 0, 3, flow_func=nx.algorithms.flow.preflow_push) - 2.0) < 1e-9)
chk("shortest_augmenting_path",
    abs(nx.maximum_flow_value(Fg, 0, 3, flow_func=nx.algorithms.flow.shortest_augmenting_path) - 2.0) < 1e-9)
# Min-cost flow: single path 0->1->2, ship 1 unit, edge weights 1 each -> total cost 2.
Cg = nx.DiGraph()
Cg.add_edge(0, 1, capacity=1, weight=1)
Cg.add_edge(1, 2, capacity=1, weight=1)
Cg.nodes[0]["demand"] = -1
Cg.nodes[2]["demand"] = 1
chk("min_cost_flow_cost", nx.min_cost_flow_cost(Cg) == 2)
mcf = nx.min_cost_flow(Cg)
chk("min_cost_flow", mcf[0][1] == 1 and mcf[1][2] == 1)

# ---------------------------------------------------------------- coloring
gc_k4 = nx.greedy_color(nx.complete_graph(4), strategy="largest_first")
chk("greedy_color_k4", len(set(gc_k4.values())) == 4)  # chromatic number of K4 = 4
gc_c4 = nx.greedy_color(nx.cycle_graph(4), strategy="largest_first")
chk("greedy_color_even_cycle", len(set(gc_c4.values())) == 2)  # bipartite even cycle
gc_p5 = nx.greedy_color(nx.path_graph(5), strategy="largest_first")
chk("greedy_color_path", len(set(gc_p5.values())) == 2)
eqc = nx.coloring.equitable_color(nx.complete_graph(4), num_colors=4)
chk("equitable_color", len(set(eqc.values())) == 4)

# ---------------------------------------------------------------- dag (extra)
diamond = nx.DiGraph([(0, 1), (0, 2), (1, 3), (2, 3)])
chk("ancestors", nx.ancestors(diamond, 3) == {0, 1, 2})
chk("descendants", nx.descendants(diamond, 0) == {1, 2, 3})
lp = nx.dag_longest_path(diamond)
chk("dag_longest_path", len(lp) == 3 and lp[0] == 0 and lp[-1] == 3)
chk("dag_longest_path_length", nx.dag_longest_path_length(diamond) == 2)
tc = nx.transitive_closure(diamond)
chk("transitive_closure", tc.has_edge(0, 3))
tg = list(nx.topological_generations(diamond))
chk("topological_generations", [sorted(g) for g in tg] == [[0], [1, 2], [3]])
ltopo = list(nx.lexicographical_topological_sort(diamond))
chk("lexicographical_topological_sort", ltopo[0] == 0 and ltopo[-1] == 3)

# ---------------------------------------------------------------- distance_measures (extra)
chk("eccentricity", nx.eccentricity(nx.path_graph(4)) == {0: 3, 1: 2, 2: 2, 3: 3})
chk("center_path", nx.center(nx.path_graph(5)) == [2])
chk("periphery_path", sorted(nx.periphery(nx.path_graph(5))) == [0, 4])
chk("barycenter_path", nx.barycenter(nx.path_graph(3)) == [1])
chk("diameter_cycle", nx.diameter(nx.cycle_graph(6)) == 3)
chk("radius_cycle", nx.radius(nx.cycle_graph(6)) == 3)

# ---------------------------------------------------------------- cycles (extra)
fc = nx.find_cycle(nx.cycle_graph(4))
chk("find_cycle", len(fc) == 4)
try:
    nx.find_cycle(nx.path_graph(4))
    _no_cycle = False
except nx.NetworkXNoCycle:
    _no_cycle = True
chk("find_cycle_none", _no_cycle)
cb = nx.cycle_basis(nx.cycle_graph(5))
chk("cycle_basis", len(cb) == 1 and len(cb[0]) == 5)
sc = list(nx.simple_cycles(nx.DiGraph([(0, 1), (1, 0)])))
chk("simple_cycles", len(sc) == 1)
chk("is_tree", nx.is_tree(nx.path_graph(4)))
chk("is_forest", nx.is_forest(nx.Graph([(0, 1), (2, 3)])))
rsc = nx.recursive_simple_cycles(nx.DiGraph([(0, 1), (1, 0)]))
chk("recursive_simple_cycles", len(rsc) == 1)

# ---------------------------------------------------------------- operators
comp = nx.complement(nx.complete_graph(4))
chk("complement_complete", comp.number_of_edges() == 0)
comp2 = nx.complement(nx.empty_graph(4))
chk("complement_empty", comp2.number_of_edges() == 6)  # -> K4
composed = nx.compose(nx.Graph([(0, 1)]), nx.Graph([(1, 2)]))
chk("compose", composed.number_of_edges() == 2 and composed.has_edge(0, 1) and composed.has_edge(1, 2))
du = nx.disjoint_union(nx.path_graph(3), nx.path_graph(3))
chk("disjoint_union", du.number_of_nodes() == 6 and du.number_of_edges() == 4)
cp = nx.cartesian_product(nx.path_graph(2), nx.path_graph(2))
chk("cartesian_product", cp.number_of_nodes() == 4 and cp.number_of_edges() == 4)  # -> C4
un = nx.union(nx.Graph([(0, 1)]), nx.Graph([(2, 3)]))
chk("union", un.number_of_nodes() == 4 and un.number_of_edges() == 2)
Gi1 = nx.Graph([(0, 1), (1, 2)])
Gi2 = nx.Graph([(1, 2), (2, 3)])
Gi2.add_nodes_from([0])
Gi1.add_nodes_from([3])
inter = nx.intersection(Gi1, Gi2)
chk("intersection", list(inter.edges()) == [(1, 2)])
diff = nx.difference(Gi1, Gi2)
chk("difference", (0, 1) in diff.edges() or (1, 0) in diff.edges())
tp = nx.tensor_product(nx.path_graph(2), nx.path_graph(2))
chk("tensor_product", tp.number_of_nodes() == 4)
sp = nx.strong_product(nx.path_graph(2), nx.path_graph(2))
chk("strong_product", sp.number_of_nodes() == 4 and sp.number_of_edges() == 6)

# ---------------------------------------------------------------- isomorphism
chk("is_isomorphic", nx.is_isomorphic(nx.cycle_graph(4), nx.cycle_graph(4)))
chk("not_isomorphic", not nx.is_isomorphic(nx.path_graph(4), nx.star_graph(3)))
chk("could_be_isomorphic", nx.could_be_isomorphic(nx.complete_graph(4), nx.complete_graph(4)))
chk("faster_could_be_isomorphic", nx.faster_could_be_isomorphic(nx.cycle_graph(4), nx.cycle_graph(4)))
chk("fast_could_be_isomorphic", nx.fast_could_be_isomorphic(nx.cycle_graph(4), nx.cycle_graph(4)))
relabeled = nx.relabel_nodes(nx.path_graph(4), {0: 10, 1: 11, 2: 12, 3: 13})
chk("is_isomorphic_relabeled", nx.is_isomorphic(nx.path_graph(4), relabeled))
chk("vf2pp_is_isomorphic", nx.vf2pp_is_isomorphic(nx.cycle_graph(4), nx.cycle_graph(4)))

# ---------------------------------------------------------------- tree
Gmt = nx.Graph()
Gmt.add_weighted_edges_from([(0, 1, 1.0), (1, 2, 2.0), (0, 2, 3.0), (2, 3, 4.0)])
maxst = nx.maximum_spanning_tree(Gmt)
# Max spanning tree picks heaviest cycle-free set: {2-3=4, 0-2=3, 1-2=2} = 9.
chk("maximum_spanning_tree", abs(maxst.size(weight="weight") - 9.0) < 1e-9)
chk("is_tree_mst", nx.is_tree(nx.minimum_spanning_tree(Gmt)))
dtree = nx.dfs_tree(nx.path_graph(4), 0)
chk("dfs_tree", list(dtree.edges()) == [(0, 1), (1, 2), (2, 3)])
chk("is_arborescence", nx.is_arborescence(nx.dfs_tree(nx.path_graph(3), 0)))

# ---------------------------------------------------------------- cliques + matching (extra)
# graph_clique_number / graph_number_of_cliques were removed in networkx 3.0; test them only
# where present (guarded), otherwise reproduce the same value via find_cliques so coverage holds.
_k4cliques = list(nx.find_cliques(nx.complete_graph(4)))
if hasattr(nx, "graph_clique_number"):
    chk("graph_clique_number", nx.graph_clique_number(nx.complete_graph(4)) == 4)
    chk("graph_number_of_cliques", nx.graph_number_of_cliques(nx.complete_graph(4)) == 1)
else:
    chk("graph_clique_number", max(len(c) for c in _k4cliques) == 4)
    chk("graph_number_of_cliques", len(_k4cliques) == 1)
chk("node_clique_number", nx.node_clique_number(nx.complete_graph(4), 0) == 4)
fcr = list(nx.find_cliques_recursive(nx.complete_graph(4)))
chk("find_cliques_recursive", len(fcr) == 1 and sorted(fcr[0]) == [0, 1, 2, 3])
P4m = nx.path_graph(4)
chk("is_matching", nx.is_matching(P4m, {(0, 1), (2, 3)}))
chk("is_maximal_matching", nx.is_maximal_matching(P4m, {(0, 1), (2, 3)}))
chk("is_perfect_matching", nx.is_perfect_matching(P4m, {(0, 1), (2, 3)}))
mwm = nx.min_weight_matching(nx.Graph([(0, 1), (2, 3), (1, 2)]))
chk("min_weight_matching", nx.is_matching(nx.Graph([(0, 1), (2, 3), (1, 2)]), mwm))

# ---------------------------------------------------------------- bipartite
from networkx.algorithms import bipartite as bp
chk("is_bipartite_even", nx.is_bipartite(nx.cycle_graph(4)))
chk("is_bipartite_odd", not nx.is_bipartite(nx.cycle_graph(5)))
chk("bipartite_is_bipartite", bp.is_bipartite(nx.cycle_graph(4)))
cbg23 = nx.complete_bipartite_graph(2, 3)
s1, s2 = bp.sets(cbg23)
chk("bipartite_sets", sorted([len(s1), len(s2)]) == [2, 3])
bcolor = bp.color(cbg23)
chk("bipartite_color", len(set(bcolor.values())) == 2)
proj = bp.projected_graph(cbg23, [0, 1])  # left nodes both connect to all right -> they share
chk("bipartite_projected", proj.number_of_nodes() == 2)
chk("bipartite_density", abs(bp.density(cbg23, [0, 1]) - 1.0) < 1e-9)
hk = bp.hopcroft_karp_matching(cbg23, top_nodes=[0, 1])
chk("hopcroft_karp", len(hk) >= 2)
bmm = bp.maximum_matching(cbg23, top_nodes=[0, 1])
chk("bipartite_maximum_matching", len(bmm) >= 4)  # matched pairs counted both directions

# ---------------------------------------------------------------- assortativity
chk("degree_assortativity_star", nx.degree_assortativity_coefficient(nx.star_graph(4)) < 0)
avnd = nx.average_neighbor_degree(nx.star_graph(4))
chk("average_neighbor_degree", all(abs(avnd[i] - 4.0) < 1e-9 for i in range(1, 5)))
pear = nx.degree_pearson_correlation_coefficient(nx.path_graph(5))
chk("degree_pearson", np.isfinite(pear))
Gaa = nx.Graph([(0, 1), (2, 3)])
nx.set_node_attributes(Gaa, {0: "a", 1: "a", 2: "b", 3: "b"}, name="grp")
chk("attribute_assortativity", nx.attribute_assortativity_coefficient(Gaa, "grp") > 0)

# ---------------------------------------------------------------- planarity
is_p4, _ = nx.check_planarity(nx.complete_graph(4))
chk("check_planarity_k4", is_p4)
is_p5, _ = nx.check_planarity(nx.complete_graph(5))
chk("check_planarity_k5", not is_p5)
chk("is_planar_cycle", nx.is_planar(nx.cycle_graph(6)))

# ---------------------------------------------------------------- covering / dominating
dset = nx.dominating_set(nx.star_graph(4))
chk("dominating_set", nx.is_dominating_set(nx.star_graph(4), dset))
mec = nx.min_edge_cover(nx.path_graph(4))
chk("min_edge_cover", nx.is_edge_cover(nx.path_graph(4), mec))
from networkx.algorithms.approximation import min_weighted_vertex_cover
mwvc = min_weighted_vertex_cover(nx.cycle_graph(4))
chk("min_weighted_vertex_cover", all(u in mwvc or v in mwvc for u, v in nx.cycle_graph(4).edges()))

# ---------------------------------------------------------------- convert (round-trips)
Ain = np.array([[0, 1, 0], [1, 0, 1], [0, 1, 0]])
Aout = nx.to_numpy_array(nx.from_numpy_array(Ain))
chk("to_numpy_array_roundtrip", np.array_equal(Aout, Ain.astype(float)))
dol = nx.to_dict_of_lists(nx.path_graph(3))
chk("to_dict_of_lists", {k: sorted(v) for k, v in dol.items()} == {0: [1], 1: [0, 2], 2: [1]})
Gdol = nx.from_dict_of_lists({0: [1], 1: [0, 2], 2: [1]})
chk("from_dict_of_lists", Gdol.number_of_edges() == 2)
dod = nx.to_dict_of_dicts(nx.path_graph(3))
Gdod = nx.from_dict_of_dicts(dod)
chk("dict_of_dicts_roundtrip", set(map(frozenset, Gdod.edges())) == {frozenset((0, 1)), frozenset((1, 2))})
sp_arr = nx.to_scipy_sparse_array(nx.path_graph(3))
chk("to_scipy_sparse_array", sp_arr.nnz == 2 * 2)  # undirected -> both directions
Gsp = nx.from_scipy_sparse_array(sp_arr)
chk("from_scipy_sparse_array", Gsp.number_of_edges() == 2)
el = nx.to_edgelist(nx.path_graph(3))
Gel = nx.from_edgelist([(u, v) for u, v, _ in el])
chk("to_edgelist_roundtrip", set(map(frozenset, Gel.edges())) == {frozenset((0, 1)), frozenset((1, 2))})

# ---------------------------------------------------------------- readwrite (string round-trips, no files)
Grw = nx.path_graph(3)
adjl = list(nx.generate_adjlist(Grw))
Gadj = nx.parse_adjlist(adjl, nodetype=int)
chk("adjlist_roundtrip", set(map(frozenset, Gadj.edges())) == {frozenset((0, 1)), frozenset((1, 2))})
edl = list(nx.generate_edgelist(Grw, data=False))
Gedl = nx.parse_edgelist(edl, nodetype=int)
chk("edgelist_roundtrip", set(map(frozenset, Gedl.edges())) == {frozenset((0, 1)), frozenset((1, 2))})
# The edges= kwarg (replacing the deprecated link=) exists on networkx >= 3.4; fall back on older.
try:
    nld = nx.node_link_data(Grw, edges="edges")
    Gnld = nx.node_link_graph(nld, edges="edges")
except TypeError:
    nld = nx.node_link_data(Grw)
    Gnld = nx.node_link_graph(nld)
chk("node_link_roundtrip", Gnld.number_of_nodes() == 3 and Gnld.number_of_edges() == 2)
gml = list(nx.generate_gml(Grw))
Ggml = nx.parse_gml(gml)
chk("gml_roundtrip", Ggml.number_of_nodes() == 3 and Ggml.number_of_edges() == 2)
adjd_rw = nx.adjacency_data(Grw)
Gadjd = nx.adjacency_graph(adjd_rw)
chk("adjacency_data_roundtrip", Gadjd.number_of_nodes() == 3 and Gadjd.number_of_edges() == 2)
import io as _io
gml_buf = _io.BytesIO()
nx.write_graphml(Grw, gml_buf)
gml_buf.seek(0)
Ggraphml = nx.read_graphml(gml_buf, node_type=int)
chk("graphml_stringio_roundtrip", Ggraphml.number_of_nodes() == 3 and Ggraphml.number_of_edges() == 2)

# ---------------------------------------------------------------- shortest_path extras
apspl = dict(nx.all_pairs_shortest_path_length(nx.path_graph(3)))
chk("all_pairs_shortest_path_length", apspl[0] == {0: 0, 1: 1, 2: 2})
apdpl = dict(nx.all_pairs_dijkstra_path_length(nx.path_graph(3)))
chk("all_pairs_dijkstra_path_length", apdpl[0] == {0: 0, 1: 1, 2: 2})
chk("average_shortest_path_length", abs(nx.average_shortest_path_length(nx.complete_graph(4)) - 1.0) < 1e-9)
asp = list(nx.all_shortest_paths(nx.cycle_graph(4), 0, 2))
chk("all_shortest_paths", len(asp) == 2)  # two length-2 paths around the 4-cycle
Gj = nx.DiGraph()
Gj.add_weighted_edges_from([(0, 1, 1.0), (1, 2, 2.0), (0, 2, 10.0)])
joh = nx.johnson(Gj)
chk("johnson", joh[0][2] == [0, 1, 2])
ssd_len, ssd_path = nx.single_source_dijkstra(Gj, 0)
chk("single_source_dijkstra", abs(ssd_len[2] - 3.0) < 1e-9 and ssd_path[2] == [0, 1, 2])
chk("negative_edge_cycle", not nx.negative_edge_cycle(Gj)
    and nx.negative_edge_cycle(nx.DiGraph([(0, 1, {"weight": -1}), (1, 0, {"weight": -1})])))

# ---------------------------------------------------------------- traversal extras
chk("dfs_postorder", list(nx.dfs_postorder_nodes(nx.path_graph(4), 0)) == [3, 2, 1, 0])
bsucc = dict(nx.bfs_successors(nx.star_graph(3), 0))
chk("bfs_successors", sorted(bsucc[0]) == [1, 2, 3])
bpred = dict(nx.bfs_predecessors(nx.path_graph(4), 0))
chk("bfs_predecessors", bpred == {1: 0, 2: 1, 3: 2})
dsucc = nx.dfs_successors(nx.path_graph(4), 0)
chk("dfs_successors", dsucc == {0: [1], 1: [2], 2: [3]})
dpred = nx.dfs_predecessors(nx.path_graph(4), 0)
chk("dfs_predecessors", dpred == {1: 0, 2: 1, 3: 2})
dad = nx.descendants_at_distance(nx.path_graph(5), 0, 2)
chk("descendants_at_distance", dad == {2})
edfs = list(nx.edge_dfs(nx.path_graph(3), 0))
chk("edge_dfs", edfs == [(0, 1), (1, 2)])
ebfs = list(nx.edge_bfs(nx.path_graph(3), 0))
chk("edge_bfs", ebfs == [(0, 1), (1, 2)])

# ---------------------------------------------------------------- HONEST-SKIP (documented, not asserted)
# smallworld (sigma/omega): require many seeded random rewirings + niter iterations -> too slow
#   under softfloat TCG on loong, and results are statistical (not closed-form). Skipped for determinism/time.
# similarity (graph_edit_distance / optimize_*): NP-hard, exponential blow-up even on tiny graphs
#   and can hang -> unsafe for single-core TCG budget. Skipped.

print("NETWORKX_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("NETWORKX_DONE")
    sys.exit(0)
sys.exit(1)
