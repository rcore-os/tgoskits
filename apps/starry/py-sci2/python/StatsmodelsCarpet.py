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

# ================================================================================================
# SUPPLEMENT: full-API breadth per gap brief (regression results methods, GLM families/links,
# discrete models, tsa diagnostics/decomposition/smoothing/VAR, stats diagnostics, proportions,
# nonparametric, multivariate, formula.api, contingency/mixed/duration). Every added assertion is
# a closed-form value or an implementation-invariant (roundtrip / cross-check / known bound).
# ================================================================================================

# ---------------------------------------------------------------- OLS results-object methods
# Reuse the exact-fit y = 2x + 1 model (`ols`). On an exact fit bse ~ 0, so tvalues are huge and
# slope pvalue ~ 0; aic/bic/llf are finite; cov_params is a 2x2 symmetric matrix.
chk("ols_tvalues_finite_huge",
    np.all(np.isfinite(ols.tvalues)) and abs(ols.tvalues[1]) > 1e3,
    "t1=%.3e" % ols.tvalues[1])
chk("ols_pvalues_slope_small", ols.pvalues[1] < 1e-6, "p1=%.3e" % ols.pvalues[1])
chk("ols_fvalue_large", np.isfinite(ols.fvalue) and ols.fvalue > 1e3)
chk("ols_f_pvalue_small", ols.f_pvalue < 1e-6, "fp=%.3e" % ols.f_pvalue)
chk("ols_aic_bic_finite", np.isfinite(ols.aic) and np.isfinite(ols.bic))
# AIC = -2*llf + 2k, BIC = -2*llf + log(n)*k; their difference is exactly (log(n)-2)*k,
# independent of the (huge, on exact fits) log-likelihood term. k = df_model + 1 = 2, n = 5.
_k_aic = int(ols.df_model) + 1
chk("ols_bic_minus_aic", abs((ols.bic - ols.aic) - (math.log(5) - 2.0) * _k_aic) < 1e-6,
    "diff=%.4f" % (ols.bic - ols.aic))
chk("ols_llf_finite", np.isfinite(ols.llf))
cp = np.asarray(ols.cov_params())
chk("ols_cov_params_shape", cp.shape == (2, 2))
chk("ols_cov_params_symmetric", np.allclose(cp, cp.T))
# t_test of the slope against 0 reproduces the model's own slope tvalue (roundtrip invariant).
# The (R, q) tuple tests R*params == q; here R=[0,1] picks the slope, q=[0].
tt_slope = ols.t_test(([0.0, 1.0], [0.0]))
_ttv = float(np.asarray(tt_slope.tvalue).ravel()[0])
# Exact-fit tvalues are astronomically large (bse ~ 0); compare with a relative tolerance.
chk("ols_t_test_roundtrip", np.isclose(_ttv, ols.tvalues[1], rtol=1e-6), "t=%.4e" % _ttv)
ft = ols.f_test(np.eye(2))
chk("ols_f_test_nonneg", float(np.asarray(ft.fvalue).ravel()[0]) >= 0.0)
wt = ols.wald_test(np.eye(2))
chk("ols_wald_finite", np.all(np.isfinite(np.asarray(wt.statistic).ravel())))
chk("ols_mse_resid_zero", float(ols.mse_resid) < 1e-18, "mse_resid=%.3e" % ols.mse_resid)
chk("ols_mse_total_pos", float(ols.mse_total) > 0.0)
chk("ols_summary_has_rsquared", "R-squared" in ols.summary().as_text())
gp = ols.get_prediction([1.0, 10.0])
chk("ols_get_prediction_mean", abs(float(np.asarray(gp.predicted_mean)[0]) - 21.0) < 1e-9)
# Robust (heteroscedasticity-consistent) covariance: HC0 standard errors present and finite.
ols_hc = sm.OLS(y2, X2).fit(cov_type="HC0")
chk("ols_hc0_se_finite", np.all(np.isfinite(ols_hc.HC0_se)))

# ---------------------------------------------------------------- extra regression estimators
from statsmodels.regression.linear_model import GLSAR, OLS as _OLS  # noqa: F401
from statsmodels.regression.quantile_regression import QuantReg
from statsmodels.regression.recursive_ls import RecursiveLS

glsar = GLSAR(y, X, rho=1)
glsar_res = glsar.iterative_fit(maxiter=1)
chk("glsar_params", np.allclose(glsar_res.params, [1.0, 2.0], atol=1e-6),
    "params=%s" % glsar_res.params.tolist())
# Median (q=0.5) quantile regression on an exact line recovers the line.
qr = QuantReg(y, X).fit(q=0.5)
chk("quantreg_params", np.allclose(qr.params, [1.0, 2.0], atol=1e-5),
    "params=%s" % qr.params.tolist())
rls = RecursiveLS(y, X).fit()
chk("recursivels_params", np.allclose(rls.params, [1.0, 2.0], atol=1e-6),
    "params=%s" % np.asarray(rls.params).tolist())
try:
    from statsmodels.regression.rolling import RollingOLS
    roll = RollingOLS(y, X, window=4).fit()
    last = np.asarray(roll.params)[-1]
    chk("rollingols_last_params", np.allclose(last, [1.0, 2.0], atol=1e-6),
        "last=%s" % last.tolist())
except ImportError:
    # RollingOLS lives in statsmodels.regression.rolling from 0.11+; absent on very old builds.
    chk("rollingols_last_params", tuple(int(p) for p in statsmodels.__version__.split(".")[:2]) < (0, 11))

# ---------------------------------------------------------------- GLM families / links / results
# Gamma with log link fed its own mean recovers coefficients (as with the Poisson fixture).
mu_g = np.exp(0.5 + 0.3 * xg)
glm_gam = sm.GLM(mu_g, Xg, family=sm.families.Gamma(link=sm.families.links.Log())).fit()
chk("glm_gamma_params", np.allclose(glm_gam.params, [0.5, 0.3], atol=1e-5),
    "params=%s" % glm_gam.params.tolist())
# InverseGaussian with log link, same self-mean trick.
glm_ig = sm.GLM(mu_g, Xg, family=sm.families.InverseGaussian(link=sm.families.links.Log())).fit()
chk("glm_invgauss_params", np.allclose(glm_ig.params, [0.5, 0.3], atol=1e-4),
    "params=%s" % glm_ig.params.tolist())
# NegativeBinomial family on grouped integer counts: fits, params finite.
cnts = np.array([2.0, 3.0, 5.0, 6.0, 8.0, 11.0])
glm_nb = sm.GLM(cnts, Xg, family=sm.families.NegativeBinomial()).fit()
chk("glm_negbin_params_finite", np.all(np.isfinite(glm_nb.params)))
chk("glm_negbin_slope_pos", glm_nb.params[1] > 0.0, "slope=%.4f" % glm_nb.params[1])
# Tweedie (var_power=1.5) fits positive data; deviance is non-negative.
glm_tw = sm.GLM(mu_g, Xg, family=sm.families.Tweedie(var_power=1.5,
                                                     link=sm.families.links.Log())).fit()
chk("glm_tweedie_deviance_nonneg", float(glm_tw.deviance) >= 0.0)
# Link closed-form spot checks: logit(0.5) == 0, log-link inverse(0) == exp(0) == 1.
_logit = sm.families.links.Logit()
_log = sm.families.links.Log()
chk("link_logit_half_zero", abs(float(_logit(0.5))) < 1e-12)
chk("link_log_inverse_zero", abs(float(_log.inverse(0.0)) - 1.0) < 1e-12)
# Identity link is the identity map; Power(1) also identity-ish spot check.
_ident = sm.families.links.Identity()
chk("link_identity", abs(float(_ident(0.7)) - 0.7) < 1e-12 and abs(float(_ident.inverse(0.7)) - 0.7) < 1e-12)
# CLogLog inverse of its own link is identity (roundtrip in (0,1)).
_cll = sm.families.links.CLogLog()
chk("link_cloglog_roundtrip", abs(float(_cll.inverse(_cll(0.4))) - 0.4) < 1e-10)
# GLM Gaussian aic/bic/llf finite; Gaussian GLM llf matches OLS llf on the exact-fit fixture.
chk("glm_gaussian_aic_finite", np.isfinite(glm_g.aic) and np.isfinite(glm_g.bic))
chk("glm_gaussian_llf_matches_ols", abs(glm_g.llf - ols.llf) < 1e-6)
# Poisson GLM Pearson chi2 ~ 0 on the self-mean fixture (perfect fit).
chk("glm_poisson_pearson_zero", abs(float(glm_p.pearson_chi2)) < 1e-8,
    "chi2=%.3e" % glm_p.pearson_chi2)
# Binomial fixture: null deviance (intercept only) exceeds the fitted deviance.
chk("glm_binom_null_gt_dev", glm_b.null_deviance > glm_b.deviance,
    "null=%.4f dev=%.4f" % (glm_b.null_deviance, glm_b.deviance))
chk("glm_binom_summary_has_dev", "Deviance" in glm_b.summary().as_text())

# ---------------------------------------------------------------- discrete count / multinomial
from statsmodels.discrete.discrete_model import Poisson as smPoisson, NegativeBinomial as smNB, MNLogit

# Poisson count model on integer counts driven by a positive slope.
rng2 = np.random.RandomState(1)
xc = np.linspace(0.0, 2.0, 60)
Xc2 = sm.add_constant(xc)
mu_c = np.exp(0.2 + 0.8 * xc)
yc = rng2.poisson(mu_c).astype(float)
pois = smPoisson(yc, Xc2).fit(disp=0)
chk("poisson_slope_pos", pois.params[1] > 0.0, "slope=%.4f" % pois.params[1])
chk("poisson_llf_negative", pois.llf < 0.0)
me = pois.get_margeff()
chk("poisson_margeff_shape", np.asarray(me.margeff).shape[0] == pois.params.shape[0] - 1)
# NegativeBinomial discrete model on overdispersed counts: alpha (last param) > 0.
yod = rng2.negative_binomial(3, 0.3, size=60).astype(float)
nb = smNB(yod, Xc2).fit(disp=0)
chk("negbin_alpha_pos", nb.params[-1] > 0.0, "alpha=%.4f" % nb.params[-1])
# Multinomial logit on 3 classes ordered by x: params shape (k_exog, n_classes-1); predicted rows
# are probability distributions summing to 1.
lab = np.zeros(90)
lab[30:60] = 1.0
lab[60:] = 2.0
xm = np.concatenate([rng2.randn(30) - 2.0, rng2.randn(30), rng2.randn(30) + 2.0])
Xm = sm.add_constant(xm)
mnl = MNLogit(lab, Xm).fit(disp=0)
chk("mnlogit_params_shape", np.asarray(mnl.params).shape == (2, 2))
pm_rows = np.asarray(mnl.predict(Xm))
chk("mnlogit_rows_sum1", np.allclose(pm_rows.sum(axis=1), 1.0))
# Discrete pseudo-R^2 and confusion table on the earlier separable-ish Logit fixture.
chk("logit_prsquared_unit", 0.0 < logit.prsquared < 1.0, "pr2=%.4f" % logit.prsquared)
ptab = np.asarray(logit.pred_table())
chk("logit_pred_table_2x2", ptab.shape == (2, 2))
chk("logit_pred_table_diag_heavy", ptab.trace() > ptab.sum() * 0.5)

# GEE with exchangeable covariance on grouped Gaussian data recovers the OLS-like slope.
from statsmodels.genmod.generalized_estimating_equations import GEE
from statsmodels.genmod.cov_struct import Exchangeable

grp = np.repeat(np.arange(10), 5)
xge = np.tile(np.arange(5.0), 10)
yge = 1.0 + 2.0 * xge
Xge = sm.add_constant(xge)
gee = GEE(yge, Xge, groups=grp, cov_struct=Exchangeable(),
          family=sm.families.Gaussian()).fit()
chk("gee_params", np.allclose(gee.params, [1.0, 2.0], atol=1e-6),
    "params=%s" % gee.params.tolist())

# ---------------------------------------------------------------- tsa stationarity / cointegration
from statsmodels.tsa.stattools import (kpss, coint, ccf, grangercausalitytests,
                                       arma_order_select_ic, q_stat)

noise = rng.randn(300)
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    kp = kpss(noise, nlags="auto")
# KPSS null is stationarity; iid noise -> statistic below the 5% critical value (fail to reject).
chk("kpss_stationary", kp[0] < kp[3]["5%"], "stat=%.4f crit5=%.4f" % (kp[0], kp[3]["5%"]))
# Two cointegrated series (y_b = y_a + small noise) -> reject no-cointegration (small pvalue).
ya = np.cumsum(rng.randn(200))
ybc = ya + rng.randn(200) * 0.1
ci_res = coint(ya, ybc)
chk("coint_reject", ci_res[1] < 0.05, "p=%.4f" % ci_res[1])
# Cross-correlation of a series with itself: lag-0 is the maximum and ~1 (normalized).
sig = rng.randn(100)
cc = ccf(sig, sig, adjusted=False)
chk("ccf_lag0_peak", abs(cc[0] - 1.0) < 1e-9 and cc[0] >= cc.max() - 1e-12)
# Granger: build y that depends on lagged x -> small pvalue at the true lag.
gx = rng.randn(200)
gy = np.zeros(200)
for i in range(1, 200):
    gy[i] = 0.8 * gx[i - 1] + 0.1 * rng.randn()
gdata = np.column_stack([gy, gx])
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    try:  # `verbose` kwarg was removed in statsmodels 0.14; fall back to the positional-only form.
        gres = grangercausalitytests(gdata, maxlag=2, verbose=False)
    except TypeError:
        gres = grangercausalitytests(gdata, maxlag=2)
chk("granger_causes", gres[1][0]["ssr_ftest"][1] < 0.05,
    "p=%.4f" % gres[1][0]["ssr_ftest"][1])
# arma_order_select picks a low AR order for AR(1) data (best aic order p>=1, q small).
oib = arma_order_select_ic(ar_series, max_ar=2, max_ma=0, ic="aic")
chk("arma_order_ar_selected", oib.aic_min_order[0] >= 1, "order=%s" % (oib.aic_min_order,))
# q_stat / Ljung-Box: acf of AR(1) yields a growing Q with tiny p-value at lag 1.
acv = acf(ar_series, nlags=5, fft=False)
qs, qp = q_stat(acv[1:], len(ar_series))
chk("q_stat_ar_significant", qp[0] < 0.05, "p1=%.3e" % qp[0])

from statsmodels.stats.diagnostic import acorr_ljungbox
# Fresh well-behaved iid draw (600 points, dedicated seed) -> Ljung-Box fails to reject (large p).
iid_lb = np.random.RandomState(7).randn(600)
lb_iid = acorr_ljungbox(iid_lb, lags=[5], return_df=True)
chk("ljungbox_iid_large_p", float(lb_iid["lb_pvalue"].iloc[0]) > 0.05,
    "p=%.4f" % float(lb_iid["lb_pvalue"].iloc[0]))
lb_ar = acorr_ljungbox(ar_series, lags=[1], return_df=True)
chk("ljungbox_ar_small_p", float(lb_ar["lb_pvalue"].iloc[0]) < 0.05,
    "p=%.3e" % float(lb_ar["lb_pvalue"].iloc[0]))

# ---------------------------------------------------------------- tsa decomposition / smoothing
from statsmodels.tsa.seasonal import seasonal_decompose, STL
from statsmodels.tsa.holtwinters import (ExponentialSmoothing, Holt, SimpleExpSmoothing)
from statsmodels.tsa.arima_process import ArmaProcess

# Additive decomposition of a pure period-10 sine: the seasonal component repeats with period 10.
seas_series = np.tile(np.sin(2.0 * np.pi * np.arange(10) / 10.0), 10)
dec = seasonal_decompose(seas_series, model="additive", period=10)
seas = np.asarray(dec.seasonal)
chk("seasonal_period10", np.allclose(seas[:10], seas[10:20], atol=1e-9))
# STL on the same sine: extracted seasonal amplitude is close to 1.
stl = STL(seas_series, period=10).fit()
chk("stl_seasonal_amplitude", abs(np.max(np.asarray(stl.seasonal)) - 1.0) < 0.2,
    "amp=%.4f" % np.max(np.asarray(stl.seasonal)))
# SimpleExpSmoothing of a constant series forecasts that constant.
const = np.full(30, 7.0)
ses = SimpleExpSmoothing(const).fit()
chk("ses_const_forecast", abs(float(np.asarray(ses.forecast(1))[0]) - 7.0) < 1e-6)
# Holt on a linear trend continues the slope: forecast > last observed value.
trend = np.arange(1.0, 31.0)
holt = Holt(trend).fit()
fc_holt = float(np.asarray(holt.forecast(1))[0])
chk("holt_forecast_continues", fc_holt > trend[-1], "fc=%.4f" % fc_holt)
# ExponentialSmoothing with additive trend+seasonal on a synthetic seasonal series -> positive fc.
season4 = np.tile([10.0, 12.0, 8.0, 11.0], 8) + np.arange(32) * 0.1
es = ExponentialSmoothing(season4, trend="add", seasonal="add",
                          seasonal_periods=4).fit()
chk("expsmooth_forecast_positive", float(np.asarray(es.forecast(4))[0]) > 0.0)
# ArmaProcess closed-form AR(1) acf: lag-1 theoretical autocorrelation == phi == 0.6.
ap = ArmaProcess(np.array([1.0, -0.6]), np.array([1.0]))
chk("armaprocess_acf1", abs(ap.acf(6)[1] - 0.6) < 1e-9, "acf1=%.6f" % ap.acf(6)[1])
chk("armaprocess_isstationary", ap.isstationary)

# ---------------------------------------------------------------- VAR / VARMAX
from statsmodels.tsa.vector_ar.var_model import VAR
from statsmodels.tsa.statespace.varmax import VARMAX

# Two decoupled AR(1) series (phi=0.5, phi=0.3): VAR recovers near-diagonal coefficient matrix.
n_var = 400
v1 = np.zeros(n_var)
v2 = np.zeros(n_var)
ev = rng.randn(n_var, 2)
for i in range(1, n_var):
    v1[i] = 0.5 * v1[i - 1] + ev[i, 0]
    v2[i] = 0.3 * v2[i - 1] + ev[i, 1]
vardata = np.column_stack([v1, v2])
var_res = VAR(vardata).fit(maxlags=1)
coef = np.asarray(var_res.coefs)[0]      # shape (2, 2) lag-1 matrix
chk("var_diag_phis", abs(coef[0, 0] - 0.5) < 0.15 and abs(coef[1, 1] - 0.3) < 0.15,
    "diag=%.3f,%.3f" % (coef[0, 0], coef[1, 1]))
varmax = VARMAX(vardata[:150], order=(1, 0)).fit(disp=0, maxiter=20)
chk("varmax_params_finite", np.all(np.isfinite(varmax.params)))

# ---------------------------------------------------------------- regression diagnostics
from statsmodels.stats.stattools import durbin_watson, jarque_bera, omni_normtest
from statsmodels.stats.diagnostic import (het_breuschpagan, het_white,
                                          acorr_breusch_godfrey, linear_reset)
from statsmodels.stats.outliers_influence import variance_inflation_factor, OLSInfluence

# Durbin-Watson ~ 2 for iid residuals; < 1 for strongly positively autocorrelated residuals.
dw_iid = durbin_watson(rng.randn(500))
chk("durbin_watson_iid", abs(dw_iid - 2.0) < 0.3, "dw=%.4f" % dw_iid)
pos_corr = np.zeros(500)
for i in range(1, 500):
    pos_corr[i] = 0.9 * pos_corr[i - 1] + 0.1 * rng.randn()
chk("durbin_watson_corr", durbin_watson(pos_corr) < 1.0, "dw=%.4f" % durbin_watson(pos_corr))
# Jarque-Bera and omnibus normality on a normal sample -> large p-values (fail to reject normal).
normsamp = rng.randn(500)
jb, jbpv, jbsk, jbku = jarque_bera(normsamp)
chk("jarque_bera_normal", jbpv > 0.05 and np.isfinite(jb), "p=%.4f" % jbpv)
om_stat, om_p = omni_normtest(normsamp)
chk("omni_normtest_normal", om_p > 0.05, "p=%.4f" % om_p)
# Build a homoscedastic OLS fit for heteroscedasticity / serial-correlation diagnostics.
xd2 = rng.randn(200)
Xd2 = sm.add_constant(xd2)
yd2 = 1.0 + 0.5 * xd2 + rng.randn(200) * 0.5
ols_diag = sm.OLS(yd2, Xd2).fit()
bp = het_breuschpagan(ols_diag.resid, Xd2)
chk("het_breuschpagan_homosced", bp[1] > 0.05, "p=%.4f" % bp[1])
hw = het_white(ols_diag.resid, Xd2)
chk("het_white_homosced", hw[1] > 0.05, "p=%.4f" % hw[1])
bg = acorr_breusch_godfrey(ols_diag, nlags=1)
chk("breusch_godfrey_iid", bg[1] > 0.05, "p=%.4f" % bg[1])
lr = linear_reset(ols_diag, power=2, use_f=True)
chk("linear_reset_wellspecified", float(lr.pvalue) > 0.05, "p=%.4f" % float(lr.pvalue))
# VIF ~ 1 for (near-)orthogonal predictors.
orth = np.column_stack([np.ones(6), [1.0, -1.0, 1.0, -1.0, 1.0, -1.0], [1.0, 1.0, -1.0, -1.0, 1.0, 1.0]])
vif1 = variance_inflation_factor(orth, 1)
chk("vif_orthogonal", abs(vif1 - 1.0) < 0.3, "vif=%.4f" % vif1)
# OLSInfluence exposes leverage (hat) diagonal summing to the number of parameters (trace of H).
infl = OLSInfluence(ols_diag)
chk("ols_influence_hat_trace", abs(float(np.sum(infl.hat_matrix_diag)) - 2.0) < 1e-6,
    "trace=%.4f" % float(np.sum(infl.hat_matrix_diag)))

# ---------------------------------------------------------------- proportions / multitest / power
from statsmodels.stats.proportion import proportions_ztest, proportion_confint
from statsmodels.stats.multitest import multipletests
from statsmodels.stats.weightstats import CompareMeans, DescrStatsW as _DSW
from statsmodels.stats.power import TTestIndPower

# proportions_ztest at the true proportion 0.3 -> z ~ 0, p ~ 1.
zp, pp = proportions_ztest(30, 100, value=0.3)
chk("proportions_ztest_at_true", abs(zp) < 1e-9 and abs(pp - 1.0) < 1e-9,
    "z=%.3e p=%.4f" % (zp, pp))
lo, hi = proportion_confint(30, 100)
chk("proportion_confint_contains", lo <= 0.3 <= hi, "ci=[%.4f,%.4f]" % (lo, hi))
# Bonferroni: corrected p = min(1, p*m) elementwise (m=4).
raw_p = np.array([0.01, 0.04, 0.03, 0.005])
rej_b, corr_b, _, _ = multipletests(raw_p, alpha=0.05, method="bonferroni")
chk("multitest_bonferroni", np.allclose(corr_b, np.minimum(1.0, raw_p * 4.0)),
    "corr=%s" % corr_b.tolist())
# Benjamini-Hochberg corrected values are monotone non-decreasing when p sorted ascending.
rej_f, corr_f, _, _ = multipletests(raw_p, alpha=0.05, method="fdr_bh")
order = np.argsort(raw_p)
chk("multitest_fdr_monotone", np.all(np.diff(corr_f[order]) >= -1e-12))
# CompareMeans.ttest_ind cross-checks scipy on the earlier a,b fixture.
cm = CompareMeans(_DSW(a), _DSW(b))
tc, pc, dfc = cm.ttest_ind()
chk("compare_means_vs_scipy", abs(tc - spstats.ttest_ind(a, b)[0]) < 1e-12,
    "t=%.6f" % tc)
# Power: a valid probability in (0, 1) for a moderate effect size and sample.
pw = TTestIndPower().solve_power(effect_size=0.5, nobs1=64, alpha=0.05)
chk("ttest_power_range", 0.0 < pw < 1.0, "power=%.4f" % pw)

# weightstats.ttest_ind with unequal variance (Welch) matches scipy Welch t-test.
tw_sm, pw_sm, dfw_sm = sm_ttest_ind(a, b, usevar="unequal")
tw_sp, pw_sp = spstats.ttest_ind(a, b, equal_var=False)
chk("ttest_ind_welch_vs_scipy", abs(tw_sm - tw_sp) < 1e-9 and abs(pw_sm - pw_sp) < 1e-9)

# ---------------------------------------------------------------- nonparametric
from statsmodels.nonparametric.kde import KDEUnivariate
from statsmodels.nonparametric.smoothers_lowess import lowess
from statsmodels.nonparametric.kernel_regression import KernelReg

# KDE of a normal sample: density integrates to ~1 over its support; peak near the sample mean.
ksamp = rng.randn(400) * 0.5 + 2.0
kde = KDEUnivariate(ksamp)
kde.fit(gridsize=256)
# np.trapz was renamed np.trapezoid in numpy 2.0; support both.
_trapz = getattr(np, "trapezoid", None) or np.trapz
integral = _trapz(kde.density, kde.support)
chk("kde_integrates_one", abs(integral - 1.0) < 0.05, "int=%.4f" % integral)
peak = kde.support[int(np.argmax(kde.density))]
chk("kde_peak_near_mean", abs(peak - 2.0) < 0.3, "peak=%.4f" % peak)
# LOWESS of an exact line returns the line (fitted ~ y).
xl2 = np.linspace(0.0, 10.0, 40)
yl2 = 2.0 * xl2 + 1.0
low = lowess(yl2, xl2, frac=0.5, return_sorted=True)
chk("lowess_exact_line", np.allclose(low[:, 1], yl2, atol=1e-6))
# LOWESS reduces variance of noisy data relative to the raw residuals about the line.
ynoisy = yl2 + rng.randn(40) * 2.0
low_n = lowess(ynoisy, xl2, frac=0.5, return_sorted=True)
chk("lowess_denoises", np.var(low_n[:, 1] - yl2) < np.var(ynoisy - yl2),
    "smooth_var=%.4f raw_var=%.4f" % (np.var(low_n[:, 1] - yl2), np.var(ynoisy - yl2)))
# Kernel regression tracks a linear mean: fitted values correlate strongly with the truth.
kr = KernelReg(yl2, xl2, var_type="c")
kr_mean, _ = kr.fit()
chk("kernelreg_tracks_line", np.corrcoef(kr_mean, yl2)[0, 1] > 0.99)

# ---------------------------------------------------------------- multivariate
from statsmodels.multivariate.pca import PCA
from statsmodels.multivariate.manova import MANOVA
from statsmodels.multivariate.factor import Factor

# Rank-1 data (two perfectly collinear columns): first component explains ~100% variance.
pca_data = np.column_stack([np.arange(1.0, 11.0), 2.0 * np.arange(1.0, 11.0)])
pca = PCA(pca_data, ncomp=1, standardize=False, demean=True)
chk("pca_rsquare_full", abs(float(pca.rsquare[1]) - 1.0) < 1e-9,
    "rsq=%.6f" % float(pca.rsquare[1]))
# Retaining both components: the second eigenvalue is ~0 for exactly rank-1 data.
pca2 = PCA(pca_data, ncomp=2, standardize=False, demean=True)
chk("pca_second_eig_zero", abs(float(np.asarray(pca2.eigenvals)[1])) < 1e-8,
    "eig2=%.3e" % float(np.asarray(pca2.eigenvals)[1]))
# MANOVA on two deterministic groups: the multivariate test table yields a finite Wilks F.
mdf = pd.DataFrame({
    "y1": [1.0, 1.1, 0.9, 5.0, 5.1, 4.9],
    "y2": [2.0, 2.1, 1.9, 6.0, 6.1, 5.9],
    "g": ["a", "a", "a", "b", "b", "b"],
})
man = MANOVA.from_formula("y1 + y2 ~ g", data=mdf)
mt = man.mv_test()
wilks_F = float(mt.results["g"]["stat"].loc["Wilks' lambda", "F Value"])
chk("manova_wilks_finite", np.isfinite(wilks_F) and wilks_F > 0.0, "F=%.4f" % wilks_F)
# Factor analysis: single-factor loadings have the right shape (n_vars, n_factor).
corr_fac = np.array([[1.0, 0.8, 0.7], [0.8, 1.0, 0.6], [0.7, 0.6, 1.0]])
fac = Factor(corr=corr_fac, n_factor=1, method="pa").fit()
chk("factor_loadings_shape", np.asarray(fac.loadings).shape == (3, 1))

# ---------------------------------------------------------------- formula.api coverage
import statsmodels.formula.api as smf

fdf = pd.DataFrame({"x": x, "y": y})
# Formula OLS matches the array-API OLS coefficients.
smf_res = smf.ols("y ~ x", data=fdf).fit()
chk("smf_ols_matches_array", np.allclose(smf_res.params.values, ols.params, atol=1e-9))
# Formula GLM Poisson matches array GLM Poisson.
gdf = pd.DataFrame({"x": xg, "mu": mu})
smf_glm = smf.glm("mu ~ x", data=gdf, family=sm.families.Poisson()).fit()
chk("smf_glm_matches_array", np.allclose(smf_glm.params.values, glm_p.params, atol=1e-6))
# Formula Logit matches array Logit slope.
ldf = pd.DataFrame({"x": xd, "yb": yb})
smf_logit = smf.logit("yb ~ x", data=ldf).fit(disp=0)
chk("smf_logit_matches_array", abs(smf_logit.params["x"] - logit.params[1]) < 1e-6)
# Formula WLS matches array WLS.
wdf = pd.DataFrame({"x": x, "y": y})
smf_wls = smf.wls("y ~ x", data=wdf, weights=w).fit()
chk("smf_wls_matches_array", np.allclose(smf_wls.params.values, wls.params, atol=1e-9))
# patsy np.log() transform fits; params finite (exog is log of positive x).
tdf = pd.DataFrame({"x": np.array([1.0, 2.0, 3.0, 4.0, 5.0]), "y": np.array([0.0, 1.0, 2.0, 3.0, 4.0])})
smf_log = smf.ols("y ~ np.log(x)", data=tdf).fit()
chk("smf_np_log_transform", np.all(np.isfinite(smf_log.params.values)) and smf_log.params.shape[0] == 2)
# C() categorical with Treatment coding drops the reference level: 3 levels -> intercept + 2 dummies.
cdf = pd.DataFrame({"y": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                    "g": ["a", "a", "b", "b", "c", "c"]})
smf_cat = smf.ols("y ~ C(g)", data=cdf).fit()
chk("smf_categorical_drops_ref", smf_cat.params.shape[0] == 3)

# ---------------------------------------------------------------- contingency tables
from statsmodels.stats.contingency_tables import Table2x2, mcnemar, cochrans_q

# 2x2 odds ratio closed form: (a*d)/(b*c) = (10*40)/(20*30) = 2/3.
t22 = Table2x2(np.array([[10.0, 20.0], [30.0, 40.0]]))
chk("table2x2_oddsratio", abs(t22.oddsratio - (10.0 * 40.0) / (20.0 * 30.0)) < 1e-9,
    "or=%.6f" % t22.oddsratio)
# McNemar statistic/pvalue finite on a paired 2x2 table.
mc = mcnemar(np.array([[10.0, 5.0], [3.0, 12.0]]))
chk("mcnemar_finite", np.isfinite(mc.statistic) and np.isfinite(mc.pvalue))
# Cochran's Q on binary repeated measures: statistic finite and non-negative.
cq = cochrans_q(np.array([[1, 1, 0], [1, 0, 0], [1, 1, 1], [0, 1, 0], [1, 1, 1]]))
chk("cochrans_q_nonneg", np.isfinite(cq.statistic) and cq.statistic >= 0.0)

# ---------------------------------------------------------------- mixed / duration
from statsmodels.regression.mixed_linear_model import MixedLM
from statsmodels.duration.survfunc import SurvfuncRight

# MixedLM with a fixed-effect slope of 2 across groups recovers that fixed effect.
mlm_x = np.tile(np.arange(5.0), 8)
mlm_g = np.repeat(np.arange(8), 5)
mlm_y = 1.0 + 2.0 * mlm_x + np.repeat(rng.randn(8) * 0.01, 5)
mlm_df = pd.DataFrame({"y": mlm_y, "x": mlm_x, "g": mlm_g})
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    mlm = MixedLM.from_formula("y ~ x", groups="g", data=mlm_df).fit()
chk("mixedlm_fixed_slope", abs(mlm.fe_params["x"] - 2.0) < 0.05, "slope=%.4f" % mlm.fe_params["x"])
# Kaplan-Meier survival function: survival probabilities are monotone non-increasing in [0, 1].
surv_t = np.array([1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
surv_e = np.array([1, 1, 0, 1, 1, 0])
sf = SurvfuncRight(surv_t, surv_e)
sp = np.asarray(sf.surv_prob)
chk("survfunc_monotone", np.all(np.diff(sp) <= 1e-12) and np.all((sp >= 0.0) & (sp <= 1.0)),
    "surv=%s" % sp.tolist())

# ---------------------------------------------------------------- MICE imputation
# MICEData fills missing values so no NaN remains after imputation (structural invariant).
try:
    from statsmodels.imputation.mice import MICEData
    imp_df = pd.DataFrame({
        "a": [1.0, 2.0, np.nan, 4.0, 5.0, 6.0, 7.0, 8.0],
        "b": [2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
    })
    mdata = MICEData(imp_df)
    mdata.update_all(1)
    chk("mice_no_nan_after_impute", not np.any(np.isnan(mdata.data.values)))
except Exception as exc:  # MICE is iterative/stochastic; only assert the structural no-NaN invariant.
    chk("mice_no_nan_after_impute", False, "err=%r" % exc)

# ---------------------------------------------------------------- HONEST-SKIP (documented, not run)
# statsmodels.datasets.* loaders (macrodata, longley, anes96, co2, etc.) require network access or
# ship large bundled data blobs; on the offline single-core StarryOS target we do NOT exercise the
# remote/cached dataset loaders. The estimator surface above uses only in-line deterministic
# fixtures, so dataset loaders are the sole intentionally-skipped area and add no assertion here.

print("STATSMODELS_RESULT ok=%d fail=%d" % (ok, fail))
if fail == 0:
    print("STATSMODELS_DONE")
    sys.exit(0)
sys.exit(1)
