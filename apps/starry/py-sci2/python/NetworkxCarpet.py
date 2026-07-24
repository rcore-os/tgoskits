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

print("NETWORKX_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("NETWORKX_DONE")
    sys.exit(0)
sys.exit(1)
