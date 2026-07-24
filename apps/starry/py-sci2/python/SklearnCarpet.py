#!/usr/bin/env python3
# SklearnCarpet.py - deep deterministic-assertion carpet for scikit-learn on musl-native CPython.
#
# Exhaustive sweep of the estimator families every StarryOS scientific-computing deployment must
# support: datasets (load_iris / digits / wine / breast_cancer / diabetes plus make_classification
# / blobs / regression / moons / circles), classification (Logistic / Ridge / SGD / SVC / LinearSVC
# / KNeighbors / GaussianNB / MultinomialNB / DecisionTree / RandomForest / ExtraTrees /
# GradientBoosting / AdaBoost / Bagging / MLP), regression (Linear / Ridge / Lasso / ElasticNet /
# SVR / KNN / DecisionTree / RandomForest / GradientBoosting / MLP), clustering (KMeans /
# MiniBatchKMeans / DBSCAN / Agglomerative / Spectral / MeanShift / Birch), decomposition (PCA /
# TruncatedSVD / NMF / KernelPCA / LDA), preprocessing (Standard / MinMax / Robust / OneHot /
# LabelEncoder / PolynomialFeatures), model_selection (train_test_split / KFold / cross_val_score /
# GridSearchCV), metrics (accuracy / precision / recall / f1 / roc_auc / confusion_matrix / mse /
# r2 / silhouette) and pipeline.
#
# Every estimator is seeded with random_state=0 so its fit is reproducible bit-for-bit. Structural
# facts (shapes, label counts, class vectors, nnz) are asserted exactly; scores and coefficients
# are asserted either against closed-form analytic values (StandardScaler zero-mean/unit-var,
# LinearRegression on noise-free y=2x0+3x1+1, KMeans/Agglomerative recovering well-separated blobs
# with adjusted-Rand-index 1.0) or against the fixed-seed reference within a relative tolerance so
# a newer target build agrees with the host golden. No assertion depends on print formatting or
# float repr. Self-contained ok/fail counters; prints SKLEARN_RESULT then SKLEARN_DONE only when
# fail == 0.
import sys
import warnings

warnings.filterwarnings("ignore")

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


def close(a, b, rel=1e-6, atol=1e-9):
    return abs(a - b) <= atol + rel * abs(b)


import numpy as np
import sklearn

chk("version", int(sklearn.__version__.split(".")[0]) >= 1, "sklearn=%s" % sklearn.__version__)

# ---------------------------------------------------------------- datasets
from sklearn import datasets
from sklearn.datasets import (make_classification, make_blobs, make_regression,
                              make_moons, make_circles)

iris = datasets.load_iris()
chk("iris_shape", iris.data.shape == (150, 4) and iris.target.shape == (150,))
chk("iris_classes", np.array_equal(np.unique(iris.target), [0, 1, 2]))
chk("iris_class_counts", np.array_equal(np.bincount(iris.target), [50, 50, 50]))
chk("iris_feature_names", len(iris.feature_names) == 4)

digits = datasets.load_digits()
chk("digits_shape", digits.data.shape == (1797, 64))
chk("digits_classes", np.array_equal(np.unique(digits.target), list(range(10))))
chk("digits_pixel_range", digits.data.min() == 0.0 and digits.data.max() == 16.0)

wine = datasets.load_wine()
chk("wine_shape", wine.data.shape == (178, 13))
chk("wine_class_counts", np.array_equal(np.bincount(wine.target), [59, 71, 48]))

cancer = datasets.load_breast_cancer()
chk("cancer_shape", cancer.data.shape == (569, 30))
chk("cancer_class_counts", np.array_equal(np.bincount(cancer.target), [212, 357]))

diab = datasets.load_diabetes()
chk("diabetes_shape", diab.data.shape == (442, 10))
chk("diabetes_target_continuous", diab.target.dtype.kind == "f")

Xc, yc = make_classification(n_samples=100, n_features=20, random_state=0)
chk("make_classification_shape", Xc.shape == (100, 20))
chk("make_classification_balanced", np.array_equal(np.bincount(yc), [50, 50]))

Xbl, ybl = make_blobs(n_samples=100, centers=3, random_state=0)
chk("make_blobs_shape", Xbl.shape == (100, 2))
chk("make_blobs_counts", np.array_equal(np.bincount(ybl), [34, 33, 33]))

Xrg, yrg = make_regression(n_samples=100, n_features=10, random_state=0)
chk("make_regression_shape", Xrg.shape == (100, 10) and yrg.shape == (100,))
chk("make_regression_mean", close(float(yrg.mean()), -34.111516, rel=1e-5))

Xmn, ymn = make_moons(n_samples=100, random_state=0)
chk("make_moons_shape", Xmn.shape == (100, 2))
chk("make_moons_balanced", np.array_equal(np.bincount(ymn), [50, 50]))

Xci, yci = make_circles(n_samples=100, random_state=0)
chk("make_circles_shape", Xci.shape == (100, 2))
chk("make_circles_balanced", np.array_equal(np.bincount(yci), [50, 50]))

# ---------------------------------------------------------------- model_selection: split
from sklearn.model_selection import (train_test_split, KFold, cross_val_score,
                                     GridSearchCV)

Xi, yi = datasets.load_iris(return_X_y=True)
Xtr, Xte, ytr, yte = train_test_split(Xi, yi, test_size=0.3, random_state=0)
chk("split_train_shape", Xtr.shape == (105, 4) and Xte.shape == (45, 4))
chk("split_deterministic", ytr[:5].tolist() == [1, 2, 2, 2, 2] and int(yte.sum()) == 40)
Xtr2, Xte2, _, _ = train_test_split(Xi, yi, test_size=0.3, random_state=0)
chk("split_reproducible", np.array_equal(Xtr, Xtr2))

# ---------------------------------------------------------------- classification
from sklearn.linear_model import LogisticRegression, RidgeClassifier, SGDClassifier
from sklearn.svm import SVC, LinearSVC
from sklearn.neighbors import KNeighborsClassifier
from sklearn.naive_bayes import GaussianNB, MultinomialNB
from sklearn.tree import DecisionTreeClassifier
from sklearn.ensemble import (RandomForestClassifier, ExtraTreesClassifier,
                              GradientBoostingClassifier, AdaBoostClassifier,
                              BaggingClassifier)
from sklearn.neural_network import MLPClassifier

lr = LogisticRegression(max_iter=1000, random_state=0).fit(Xtr, ytr)
chk("logreg_train", close(lr.score(Xtr, ytr), 0.9809523809523809))
chk("logreg_test", close(lr.score(Xte, yte), 0.9777777777777777))
chk("logreg_predict_shape", lr.predict(Xte).shape == (45,))
chk("logreg_proba_rows_sum1", np.allclose(lr.predict_proba(Xte).sum(1), 1.0))
chk("logreg_coef_shape", lr.coef_.shape == (3, 4))

rc = RidgeClassifier(random_state=0).fit(Xtr, ytr)
chk("ridgeclf_train", close(rc.score(Xtr, ytr), 0.8571428571428571))
chk("ridgeclf_test", close(rc.score(Xte, yte), 0.7555555555555555))

sgd = SGDClassifier(random_state=0).fit(Xtr, ytr)
chk("sgd_train", close(sgd.score(Xtr, ytr), 0.9238095238095239))
chk("sgd_test", close(sgd.score(Xte, yte), 0.8666666666666667))

svc = SVC(random_state=0).fit(Xtr, ytr)
chk("svc_train", close(svc.score(Xtr, ytr), 0.9714285714285714))
chk("svc_test", close(svc.score(Xte, yte), 0.9777777777777777))
chk("svc_n_support", int(svc.n_support_.sum()) == svc.support_vectors_.shape[0])

lsvc = LinearSVC(random_state=0, max_iter=5000).fit(Xtr, ytr)
chk("linearsvc_train", close(lsvc.score(Xtr, ytr), 0.9809523809523809))
chk("linearsvc_test", close(lsvc.score(Xte, yte), 0.9333333333333333))

knn = KNeighborsClassifier().fit(Xtr, ytr)
chk("knn_train", close(knn.score(Xtr, ytr), 0.9714285714285714))
chk("knn_test", close(knn.score(Xte, yte), 0.9777777777777777))
chk("knn_default_k", knn.n_neighbors == 5)

gnb = GaussianNB().fit(Xtr, ytr)
chk("gaussiannb_train", close(gnb.score(Xtr, ytr), 0.9428571428571428))
chk("gaussiannb_test", close(gnb.score(Xte, yte), 1.0))

mnb = MultinomialNB().fit(Xtr, ytr)
chk("multinomialnb_train", close(mnb.score(Xtr, ytr), 0.7047619047619048))
chk("multinomialnb_test", close(mnb.score(Xte, yte), 0.6))

dt = DecisionTreeClassifier(random_state=0).fit(Xtr, ytr)
chk("dtree_train_perfect", dt.score(Xtr, ytr) == 1.0)
chk("dtree_test", close(dt.score(Xte, yte), 0.9777777777777777))

rf = RandomForestClassifier(random_state=0).fit(Xtr, ytr)
chk("rf_train_perfect", rf.score(Xtr, ytr) == 1.0)
chk("rf_test", close(rf.score(Xte, yte), 0.9777777777777777))
chk("rf_n_estimators", len(rf.estimators_) == 100)
chk("rf_feature_importances_sum1", close(float(rf.feature_importances_.sum()), 1.0))

et = ExtraTreesClassifier(random_state=0).fit(Xtr, ytr)
chk("extratrees_train_perfect", et.score(Xtr, ytr) == 1.0)
chk("extratrees_test", close(et.score(Xte, yte), 0.9777777777777777))

gb = GradientBoostingClassifier(random_state=0).fit(Xtr, ytr)
chk("gradboost_train_perfect", gb.score(Xtr, ytr) == 1.0)
chk("gradboost_test", close(gb.score(Xte, yte), 0.9777777777777777))

ada = AdaBoostClassifier(random_state=0).fit(Xtr, ytr)
chk("adaboost_train_perfect", ada.score(Xtr, ytr) == 1.0)
chk("adaboost_test", close(ada.score(Xte, yte), 0.9333333333333333))

bag = BaggingClassifier(random_state=0).fit(Xtr, ytr)
chk("bagging_train", close(bag.score(Xtr, ytr), 0.9809523809523809))
chk("bagging_test", close(bag.score(Xte, yte), 0.9555555555555556))

mlp = MLPClassifier(random_state=0, max_iter=2000).fit(Xtr, ytr)
chk("mlp_train", close(mlp.score(Xtr, ytr), 0.9809523809523809))
chk("mlp_test", close(mlp.score(Xte, yte), 0.9777777777777777))

# ---------------------------------------------------------------- regression
from sklearn.linear_model import LinearRegression, Ridge, Lasso, ElasticNet
from sklearn.svm import SVR
from sklearn.neighbors import KNeighborsRegressor
from sklearn.tree import DecisionTreeRegressor
from sklearn.ensemble import RandomForestRegressor, GradientBoostingRegressor
from sklearn.neural_network import MLPRegressor

# Closed-form: noise-free y = 2*x0 + 3*x1 + 1 -> OLS recovers coefficients exactly.
Xe = np.array([[0., 0.], [1., 0.], [0., 1.], [1., 1.], [2., 1.]])
ye = 2 * Xe[:, 0] + 3 * Xe[:, 1] + 1
lin_exact = LinearRegression().fit(Xe, ye)
chk("linreg_coef_closedform", np.allclose(lin_exact.coef_, [2.0, 3.0]))
chk("linreg_intercept_closedform", close(float(lin_exact.intercept_), 1.0))
chk("linreg_r2_perfect", close(lin_exact.score(Xe, ye), 1.0))

Xd, yd = datasets.load_diabetes(return_X_y=True)
Rtr, Rte, str_, ste = train_test_split(Xd, yd, test_size=0.3, random_state=0)

lin = LinearRegression().fit(Rtr, str_)
chk("linreg_diab_train_r2", close(lin.score(Rtr, str_), 0.553937891544893))
chk("linreg_diab_test_r2", close(lin.score(Rte, ste), 0.39289927216962883))

rr = Ridge(random_state=0).fit(Rtr, str_)
chk("ridge_train_r2", close(rr.score(Rtr, str_), 0.4565351312386678, rel=1e-5))
chk("ridge_test_r2", close(rr.score(Rte, ste), 0.36518890658882316, rel=1e-5))

la = Lasso(random_state=0).fit(Rtr, str_)
chk("lasso_train_r2", close(la.score(Rtr, str_), 0.41565471174837445, rel=1e-5))

en = ElasticNet(random_state=0).fit(Rtr, str_)
chk("elasticnet_train_r2", close(en.score(Rtr, str_), 0.010751922930561151, rel=1e-4))

svr = SVR().fit(Rtr, str_)
chk("svr_train_r2", close(svr.score(Rtr, str_), 0.16329517324557056, rel=1e-5))

knr = KNeighborsRegressor().fit(Rtr, str_)
chk("knnreg_train_r2", close(knr.score(Rtr, str_), 0.6085633855128227, rel=1e-6))

dtr = DecisionTreeRegressor(random_state=0).fit(Rtr, str_)
chk("dtreereg_train_perfect", dtr.score(Rtr, str_) == 1.0)

rfr = RandomForestRegressor(random_state=0).fit(Rtr, str_)
chk("rfreg_train_r2", close(rfr.score(Rtr, str_), 0.9240169858876078, rel=1e-4))

gbr = GradientBoostingRegressor(random_state=0).fit(Rtr, str_)
chk("gbmreg_train_r2", close(gbr.score(Rtr, str_), 0.8761884696976282, rel=1e-4))

mlpr = MLPRegressor(random_state=0, max_iter=2000).fit(Rtr, str_)
chk("mlpreg_finite", np.isfinite(mlpr.score(Rtr, str_)))

# ---------------------------------------------------------------- clustering
from sklearn.cluster import (KMeans, MiniBatchKMeans, DBSCAN,
                             AgglomerativeClustering, SpectralClustering,
                             MeanShift, Birch)
from sklearn.metrics import silhouette_score, adjusted_rand_score

# Three well-separated blobs: label-invariant recovery is deterministic.
Xb3, yb3 = make_blobs(n_samples=150, centers=3, cluster_std=0.5, random_state=0)

km = KMeans(n_clusters=3, random_state=0, n_init=10).fit(Xb3)
chk("kmeans_ncluster", len(np.unique(km.labels_)) == 3)
chk("kmeans_ari_perfect", close(adjusted_rand_score(yb3, km.labels_), 1.0))
chk("kmeans_inertia", close(float(km.inertia_), 72.47601208324891, rel=1e-4))
chk("kmeans_centers_shape", km.cluster_centers_.shape == (3, 2))
chk("kmeans_predict_matches", np.array_equal(km.predict(Xb3), km.labels_))

mbk = MiniBatchKMeans(n_clusters=3, random_state=0, n_init=10).fit(Xb3)
chk("minibatch_ari_perfect", close(adjusted_rand_score(yb3, mbk.labels_), 1.0))

db = DBSCAN(eps=1.0, min_samples=5).fit(Xb3)
chk("dbscan_ncluster", len(set(db.labels_)) - (1 if -1 in db.labels_ else 0) == 3)

ag = AgglomerativeClustering(n_clusters=3).fit(Xb3)
chk("agglo_ari_perfect", close(adjusted_rand_score(yb3, ag.labels_), 1.0))

sc = SpectralClustering(n_clusters=3, random_state=0, affinity="rbf").fit(Xb3)
chk("spectral_ncluster", len(np.unique(sc.labels_)) == 3)

ms = MeanShift().fit(Xb3)
chk("meanshift_ncluster", len(np.unique(ms.labels_)) == 3)

br = Birch(n_clusters=3).fit(Xb3)
chk("birch_ari_perfect", close(adjusted_rand_score(yb3, br.labels_), 1.0))

chk("silhouette_blobs", close(silhouette_score(Xb3, km.labels_), 0.7143417068641298, rel=1e-5))

# ---------------------------------------------------------------- decomposition
from sklearn.decomposition import PCA, TruncatedSVD, NMF, KernelPCA
from sklearn.discriminant_analysis import LinearDiscriminantAnalysis

pca = PCA(n_components=2, random_state=0).fit(Xi)
chk("pca_transform_shape", pca.transform(Xi).shape == (150, 2))
chk("pca_evr", np.allclose(pca.explained_variance_ratio_, [0.9246187232017271, 0.05306648311706788]))
chk("pca_evr_descending", pca.explained_variance_ratio_[0] > pca.explained_variance_ratio_[1])
chk("pca_inverse_roundtrip",
    PCA(n_components=4, random_state=0).fit(Xi).inverse_transform(
        PCA(n_components=4, random_state=0).fit_transform(Xi)).shape == (150, 4))

tsvd = TruncatedSVD(n_components=2, random_state=0).fit(Xi)
chk("truncsvd_shape", tsvd.transform(Xi).shape == (150, 2))
chk("truncsvd_singvals_desc", tsvd.singular_values_[0] > tsvd.singular_values_[1])

nmf = NMF(n_components=2, random_state=0, max_iter=500).fit(Xi)
W = nmf.transform(Xi)
chk("nmf_shape", W.shape == (150, 2))
chk("nmf_nonneg", bool((W >= 0).all()) and bool((nmf.components_ >= 0).all()))

kpca = KernelPCA(n_components=2, random_state=0).fit(Xi)
chk("kernelpca_shape", kpca.transform(Xi).shape == (150, 2))

lda = LinearDiscriminantAnalysis(n_components=2).fit(Xi, yi)
chk("lda_transform_shape", lda.transform(Xi).shape == (150, 2))
chk("lda_score", close(lda.score(Xi, yi), 0.98))

# ---------------------------------------------------------------- preprocessing
from sklearn.preprocessing import (StandardScaler, MinMaxScaler, RobustScaler,
                                   OneHotEncoder, LabelEncoder,
                                   PolynomialFeatures)

Xp = np.array([[1., 2.], [3., 4.], [5., 6.]])
ss = StandardScaler().fit(Xp)
Xss = ss.transform(Xp)
chk("standardscaler_zero_mean", np.allclose(Xss.mean(0), 0.0))
chk("standardscaler_unit_var", np.allclose(Xss.std(0), 1.0))
chk("standardscaler_inverse", np.allclose(ss.inverse_transform(Xss), Xp))

mm = MinMaxScaler().fit_transform(Xp)
chk("minmaxscaler_min0", np.allclose(mm.min(0), 0.0))
chk("minmaxscaler_max1", np.allclose(mm.max(0), 1.0))

rob = RobustScaler().fit_transform(Xp)
chk("robustscaler_median0", np.allclose(rob[1], 0.0))

oh = OneHotEncoder(sparse_output=False).fit_transform([["a"], ["b"], ["a"], ["c"]])
chk("onehot_shape", oh.shape == (4, 3))
chk("onehot_row0", oh[0].tolist() == [1.0, 0.0, 0.0])
chk("onehot_row_sums1", np.allclose(oh.sum(1), 1.0))

le = LabelEncoder().fit(["cat", "dog", "cat", "bird"])
chk("labelencoder_classes", le.classes_.tolist() == ["bird", "cat", "dog"])
chk("labelencoder_transform", le.transform(["cat", "dog", "bird"]).tolist() == [1, 2, 0])
chk("labelencoder_inverse", le.inverse_transform([0, 1, 2]).tolist() == ["bird", "cat", "dog"])

pf = PolynomialFeatures(degree=2).fit_transform([[2., 3.]])
chk("polyfeatures", pf[0].tolist() == [1.0, 2.0, 3.0, 4.0, 6.0, 9.0])

# ---------------------------------------------------------------- model_selection: cv/grid
kf = KFold(n_splits=5, shuffle=True, random_state=0)
chk("kfold_nsplits", kf.get_n_splits() == 5)
chk("kfold_partition",
    sum(len(te) for _, te in kf.split(Xi)) == 150)

cv = cross_val_score(SVC(random_state=0), Xi, yi, cv=5)
chk("cross_val_score_shape", cv.shape == (5,))
chk("cross_val_score_mean", close(float(cv.mean()), 0.9666666666666668))

grid = GridSearchCV(SVC(random_state=0),
                    {"C": [0.1, 1, 10], "kernel": ["linear", "rbf"]}, cv=5)
grid.fit(Xi, yi)
chk("gridsearch_best_params", grid.best_params_ == {"C": 1, "kernel": "linear"})
chk("gridsearch_best_score", close(grid.best_score_, 0.98))
chk("gridsearch_ncandidates", len(grid.cv_results_["params"]) == 6)

# ---------------------------------------------------------------- metrics
from sklearn.metrics import (accuracy_score, precision_score, recall_score,
                             f1_score, roc_auc_score, confusion_matrix,
                             mean_squared_error, r2_score)

yt = [0, 1, 1, 0, 1, 0]
yp = [0, 1, 0, 0, 1, 1]
chk("accuracy_score", close(accuracy_score(yt, yp), 4.0 / 6.0))
chk("precision_score", close(precision_score(yt, yp), 2.0 / 3.0))
chk("recall_score", close(recall_score(yt, yp), 2.0 / 3.0))
chk("f1_score", close(f1_score(yt, yp), 2.0 / 3.0))
chk("confusion_matrix", confusion_matrix(yt, yp).tolist() == [[2, 1], [1, 2]])
chk("roc_auc_score", close(roc_auc_score([0, 0, 1, 1], [0.1, 0.4, 0.35, 0.8]), 0.75))
chk("mean_squared_error", close(mean_squared_error([1., 2., 3.], [1., 2., 4.]), 1.0 / 3.0))
chk("r2_score_perfect", r2_score([1., 2., 3.], [1., 2., 3.]) == 1.0)

# ---------------------------------------------------------------- pipeline
from sklearn.pipeline import Pipeline, make_pipeline

pipe = Pipeline([("sc", StandardScaler()),
                 ("clf", LogisticRegression(max_iter=1000, random_state=0))])
pipe.fit(Xi, yi)
chk("pipeline_score", close(pipe.score(Xi, yi), 0.9733333333333334))
chk("pipeline_nsteps", len(pipe.steps) == 2)

mkp = make_pipeline(StandardScaler(), SVC(random_state=0)).fit(Xi, yi)
chk("make_pipeline_score", close(mkp.score(Xi, yi), 0.9733333333333334))

print("SKLEARN_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("SKLEARN_DONE")
    sys.exit(0)
sys.exit(1)
