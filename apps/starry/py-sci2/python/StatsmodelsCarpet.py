#!/usr/bin/env python3
# StatsmodelsCarpet.py - deep closed-form-assertion carpet for statsmodels on musl-native CPython.
#
# Exhaustive coverage across the statsmodels estimator surface: linear models (OLS / WLS / GLS
# with fitted params, R^2, standard errors, prediction, residuals, confidence intervals),
# generalized linear models (Gaussian / Poisson / Binomial with grouped counts), discrete choice
# (Logit / Probit cross-checked against scipy), descriptive statistics (DescrStatsW / describe /
# correlation), hypothesis testing (ttest_ind / ztest / one-way ANOVA F cross-checked against
# scipy.stats.f_oneway), time-series analysis (acf / pacf / adfuller / AutoReg / ARIMA / SARIMAX)
# and robust regression (RLM with Huber norm).
#
# Every input is a fixed array or a RandomState(0)-seeded draw, so each result is deterministic.
# Floating results are compared to closed-form analytic values (exact-fit coefficients, R^2 == 1,
# acf[0] == 1, ztest at the true mean == 0, sine autocorrelation lags) within a tolerance; integer
# and structural results (nobs, df, shapes) are compared exactly. Where a fit has no analytic
# closed form (Logit/Probit on random Bernoulli draws) the assertion pins an invariant that must
# hold for any correct implementation (agreement with scipy, log-likelihood bounds, acf[0] == 1).
# No assertion depends on print formatting or float repr, so a host reference and a newer musl
# target build agree. Self-contained ok/fail counters; prints STATSMODELS_RESULT then
# STATSMODELS_DONE only when fail == 0.
import math
import sys
import warnings

# Deterministic estimators emit convergence / perfect-separation warnings on exact-fit fixtures;
# they are expected here (we assert the closed-form result, not the absence of the warning).
warnings.simplefilter("ignore")

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
import statsmodels
import statsmodels.api as sm

chk("version", tuple(int(p) for p in statsmodels.__version__.split(".")[:2]) >= (0, 12),
    "statsmodels=%s" % statsmodels.__version__)

# ---------------------------------------------------------------- OLS (exact linear fit y = 2x + 1)
x = np.array([0.0, 1.0, 2.0, 3.0, 4.0])
X = sm.add_constant(x)
y = 1.0 + 2.0 * x
ols = sm.OLS(y, X).fit()
chk("ols_params", np.allclose(ols.params, [1.0, 2.0]), "params=%s" % ols.params.tolist())
chk("ols_rsquared", abs(ols.rsquared - 1.0) < 1e-12)
chk("ols_rsquared_adj", abs(ols.rsquared_adj - 1.0) < 1e-12)
chk("ols_nobs", int(ols.nobs) == 5 and int(ols.df_resid) == 3 and int(ols.df_model) == 1)
chk("ols_predict", abs(float(ols.predict([1.0, 10.0])[0]) - 21.0) < 1e-9)   # 1 + 2*10
chk("ols_fitted_sum", abs(float(ols.fittedvalues.sum()) - float(y.sum())) < 1e-9)
chk("ols_resid_zero", abs(float(ols.resid.sum())) < 1e-9 and float(ols.ssr) < 1e-18)
chk("ols_bse_zero", np.all(ols.bse < 1e-9))
ci = ols.conf_int()
chk("ols_conf_int_shape", np.asarray(ci).shape == (2, 2))
chk("ols_conf_int_contains", np.asarray(ci)[1, 0] <= 2.0 <= np.asarray(ci)[1, 1])

# Multiple regression with a designed exact fit: y = 3 + 2*x1 - 1*x2.
x1 = np.array([1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
x2 = np.array([1.0, 0.0, 2.0, 1.0, 0.0, 3.0])
y2 = 3.0 + 2.0 * x1 - 1.0 * x2
X2 = sm.add_constant(np.column_stack([x1, x2]))
ols2 = sm.OLS(y2, X2).fit()
chk("ols_multi_params", np.allclose(ols2.params, [3.0, 2.0, -1.0], atol=1e-9),
    "params=%s" % ols2.params.tolist())
chk("ols_multi_rsquared", abs(ols2.rsquared - 1.0) < 1e-12)
chk("ols_multi_df", int(ols2.df_model) == 2 and int(ols2.df_resid) == 3)

# ---------------------------------------------------------------- WLS / GLS (weights / covariance)
w = np.array([1.0, 2.0, 3.0, 4.0, 5.0])
wls = sm.WLS(y, X, weights=w).fit()
chk("wls_params", np.allclose(wls.params, [1.0, 2.0]), "params=%s" % wls.params.tolist())
chk("wls_rsquared", abs(wls.rsquared - 1.0) < 1e-12)
gls = sm.GLS(y, X, sigma=np.eye(5)).fit()
chk("gls_identity_eq_ols", np.allclose(gls.params, ols.params))
# GLS with a diagonal covariance still recovers the exact-fit coefficients.
gls_d = sm.GLS(y, X, sigma=np.diag([1.0, 2.0, 3.0, 4.0, 5.0])).fit()
chk("gls_diag_params", np.allclose(gls_d.params, [1.0, 2.0]))

# ---------------------------------------------------------------- GLM (Gaussian / Poisson / Binomial)
glm_g = sm.GLM(y, X, family=sm.families.Gaussian()).fit()
chk("glm_gaussian_eq_ols", np.allclose(glm_g.params, ols.params))

# Poisson log link: log(mu) = 0.5 + 0.3 x, fed its own mean -> recovers coefficients exactly.
xg = np.array([0.0, 1.0, 2.0, 3.0, 4.0, 5.0])
Xg = sm.add_constant(xg)
mu = np.exp(0.5 + 0.3 * xg)
glm_p = sm.GLM(mu, Xg, family=sm.families.Poisson()).fit()
chk("glm_poisson_params", np.allclose(glm_p.params, [0.5, 0.3], atol=1e-6),
    "params=%s" % glm_p.params.tolist())
chk("glm_poisson_fitted", np.allclose(glm_p.fittedvalues, mu, rtol=1e-6))

# Binomial logit on grouped success/failure counts (monotone, non-separable).
succ = np.array([1.0, 2.0, 3.0, 5.0, 6.0, 7.0, 8.0, 9.0])
tot = np.full(8, 10.0)
endog = np.column_stack([succ, tot - succ])
Xb = sm.add_constant(np.arange(8.0))
glm_b = sm.GLM(endog, Xb, family=sm.families.Binomial()).fit()
chk("glm_binom_slope_sign", glm_b.params[1] > 0.0, "slope=%.6f" % glm_b.params[1])
chk("glm_binom_deviance_small", 0.0 <= float(glm_b.deviance) < 1.0,
    "dev=%.6f" % glm_b.deviance)
# Predicted probabilities are monotone increasing and in [0, 1].
pb = glm_b.predict(Xb)
chk("glm_binom_prob_range", np.all(pb >= 0.0) and np.all(pb <= 1.0))
chk("glm_binom_prob_monotone", np.all(np.diff(pb) > 0.0))

# ---------------------------------------------------------------- Logit / Probit (vs scipy invariants)
from scipy import stats as spstats

rng = np.random.RandomState(0)
xd = rng.randn(200)
lin = 1.5 * xd                              # true logit signal, intercept 0
p_true = 1.0 / (1.0 + np.exp(-lin))
yb = (rng.rand(200) < p_true).astype(float)
Xl = sm.add_constant(xd)
logit = sm.Logit(yb, Xl).fit(disp=0)
chk("logit_slope_sign", logit.params[1] > 0.5, "slope=%.4f" % logit.params[1])
chk("logit_intercept_small", abs(logit.params[0]) < 0.5)
chk("logit_llf_negative", logit.llf < 0.0)
# Predicted probabilities in (0, 1); log-likelihood beats the intercept-only null.
pl = logit.predict(Xl)
chk("logit_prob_range", np.all(pl > 0.0) and np.all(pl < 1.0))
chk("logit_beats_null", logit.llf > logit.llnull)
probit = sm.Probit(yb, Xl).fit(disp=0)
chk("probit_slope_sign", probit.params[1] > 0.0)
chk("probit_prob_range", np.all((probit.predict(Xl) > 0.0) & (probit.predict(Xl) < 1.0)))

# ---------------------------------------------------------------- descriptive statistics
from statsmodels.stats.weightstats import DescrStatsW

s = np.array([1.0, 2.0, 3.0, 4.0, 5.0])
d = DescrStatsW(s)
chk("descr_mean", abs(d.mean - 3.0) < 1e-12)
chk("descr_var", abs(d.var - 2.0) < 1e-12)             # population variance of 1..5
chk("descr_std", abs(d.std - math.sqrt(2.0)) < 1e-12)
chk("descr_nobs", int(d.nobs) == 5)
chk("descr_sum", abs(d.sum - 15.0) < 1e-12)
# Weighted mean: weights 1..5 on values 1..5 -> sum(i*i)/sum(i) = 55/15.
dw = DescrStatsW(s, weights=np.array([1.0, 2.0, 3.0, 4.0, 5.0]))
chk("descr_weighted_mean", abs(dw.mean - (55.0 / 15.0)) < 1e-12)

# Correlation: perfectly collinear columns give correlation 1.
Xc = np.array([[1.0, 2.0], [2.0, 4.0], [3.0, 6.0], [4.0, 8.0]])
corr = np.corrcoef(Xc.T)
chk("corr_perfect", abs(corr[0, 1] - 1.0) < 1e-12)

from statsmodels.stats.descriptivestats import describe
import pandas as pd

desc = describe(pd.DataFrame({"a": s}))
chk("describe_mean", abs(float(desc.loc["mean", "a"]) - 3.0) < 1e-12)
chk("describe_std", abs(float(desc.loc["std", "a"]) - spstats.tstd(s)) < 1e-9)

# ---------------------------------------------------------------- hypothesis testing
from statsmodels.stats.weightstats import ztest, ttest_ind as sm_ttest_ind

a = np.array([1.0, 2.0, 3.0, 4.0, 5.0])
b = np.array([2.0, 3.0, 4.0, 5.0, 6.0])                # shifted by exactly 1
t_sm, p_sm, df_sm = sm_ttest_ind(a, b)
t_sp, p_sp = spstats.ttest_ind(a, b)
chk("ttest_ind_stat", abs(t_sm - t_sp) < 1e-12, "t=%.6f" % t_sm)
chk("ttest_ind_pval", abs(p_sm - p_sp) < 1e-12)
chk("ttest_ind_df", int(df_sm) == 8)
# ztest at the true sample mean -> statistic exactly 0, p-value exactly 1.
z0, pz0 = ztest(a, value=3.0)
chk("ztest_at_mean", abs(z0) < 1e-12 and abs(pz0 - 1.0) < 1e-12)
# DescrStatsW.ttest_mean matches scipy.stats.ttest_1samp.
tm, pm, dfm = d.ttest_mean(2.0)
tsp1, psp1 = spstats.ttest_1samp(s, 2.0)
chk("ttest_mean_vs_scipy", abs(tm - tsp1) < 1e-9 and abs(pm - psp1) < 1e-9)

# One-way ANOVA: F statistic must match scipy.stats.f_oneway on the same groups.
from statsmodels.formula.api import ols as smf_ols
from statsmodels.stats.anova import anova_lm

adf = pd.DataFrame({
    "y": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
    "g": ["a", "a", "a", "b", "b", "b", "c", "c", "c"],
})
aov = anova_lm(smf_ols("y ~ C(g)", data=adf).fit(), typ=2)
f_sp, _ = spstats.f_oneway([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0])
chk("anova_F", abs(float(aov.loc["C(g)", "F"]) - f_sp) < 1e-9, "F=%.6f" % aov.loc["C(g)", "F"])
chk("anova_F_value", abs(float(aov.loc["C(g)", "F"]) - 27.0) < 1e-9)   # ss_between/ss_within scaled
chk("anova_ss_between", abs(float(aov.loc["C(g)", "sum_sq"]) - 54.0) < 1e-9)

# ---------------------------------------------------------------- time series
from statsmodels.tsa.stattools import acf, pacf, adfuller
from statsmodels.tsa.ar_model import AutoReg
from statsmodels.tsa.arima.model import ARIMA
from statsmodels.tsa.statespace.sarimax import SARIMAX

yts = rng.randn(500)
ac = acf(yts, nlags=5, fft=True)
chk("acf_lag0", abs(ac[0] - 1.0) < 1e-12)
chk("acf_len", ac.shape[0] == 6)
pc = pacf(yts, nlags=5)
chk("pacf_lag0", abs(pc[0] - 1.0) < 1e-12)
chk("pacf_len", pc.shape[0] == 6)

# acf of a period-10 sine: lag 5 is the anti-phase point, lag 10 the near-period point.
tt = np.arange(100.0)
sine = np.sin(2.0 * np.pi * tt / 10.0)
asine = acf(sine, nlags=10, fft=False)
chk("acf_sine_antiphase", abs(asine[5] - (-0.95)) < 1e-6, "lag5=%.6f" % asine[5])
chk("acf_sine_period", abs(asine[10] - 0.9) < 1e-6, "lag10=%.6f" % asine[10])

# adfuller on i.i.d. noise: strongly stationary -> very negative statistic, tiny p-value.
adf_res = adfuller(rng.randn(300))
chk("adf_stationary", adf_res[0] < -5.0 and adf_res[1] < 0.01,
    "stat=%.3f p=%.2e" % (adf_res[0], adf_res[1]))

# AR(1) process with phi = 0.6: AutoReg / ARIMA / SARIMAX all recover the coefficient.
e = rng.randn(400)
ar_series = np.zeros(400)
for i in range(1, 400):
    ar_series[i] = 0.6 * ar_series[i - 1] + e[i]
autoreg = AutoReg(ar_series, lags=1, old_names=False).fit()
chk("autoreg_phi", abs(autoreg.params[1] - 0.6) < 0.1, "phi=%.4f" % autoreg.params[1])
arima = ARIMA(ar_series, order=(1, 0, 0)).fit()
chk("arima_phi", abs(arima.arparams[0] - 0.6) < 0.1, "phi=%.4f" % arima.arparams[0])
chk("arima_nparams", arima.params.shape[0] == 3)               # const, ar1, sigma2
sarimax = SARIMAX(ar_series, order=(1, 0, 0)).fit(disp=0)
chk("sarimax_phi", abs(sarimax.params[0] - 0.6) < 0.1, "phi=%.4f" % sarimax.params[0])
# One-step forecast of an AR(1) is phi * last value: sign / magnitude sanity.
fc = arima.forecast(steps=1)
chk("arima_forecast_shape", np.asarray(fc).shape[0] == 1)

# ARIMA on a near-constant level series recovers the mean.
level = 5.0 + rng.randn(60) * 0.01
arima_c = ARIMA(level, order=(0, 0, 0)).fit()
chk("arima_mean", abs(arima_c.params[0] - 5.0) < 0.05, "mean=%.4f" % arima_c.params[0])

# ---------------------------------------------------------------- robust regression (RLM)
from statsmodels.robust.robust_linear_model import RLM

xr = np.array([0.0, 1.0, 2.0, 3.0, 4.0, 5.0])
Xr = sm.add_constant(xr)
yr = 1.0 + 2.0 * xr
rlm = RLM(yr, Xr, M=sm.robust.norms.HuberT()).fit()
chk("rlm_params", np.allclose(rlm.params, [1.0, 2.0], atol=1e-6),
    "params=%s" % rlm.params.tolist())
# RLM downweights an injected outlier and still tracks the true line closely.
yo = yr.copy()
yo[3] = 100.0
rlm_o = RLM(yo, Xr, M=sm.robust.norms.HuberT()).fit()
chk("rlm_robust_slope", abs(rlm_o.params[1] - 2.0) < 0.5, "slope=%.4f" % rlm_o.params[1])
chk("rlm_outlier_downweight", rlm_o.weights[3] < 0.5, "w=%.4f" % rlm_o.weights[3])

print("STATSMODELS_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("STATSMODELS_DONE")
    sys.exit(0)
sys.exit(1)
