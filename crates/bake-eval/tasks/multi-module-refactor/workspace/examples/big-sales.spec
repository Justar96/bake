# Regions by their sales of 100 or more.
title: Large sales by region
group-by: region
column: amount
aggregates: count, sum, p90
where: amount >= 100
sort: region
